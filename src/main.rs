use aws_sdk_sts::config::Region;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs::OpenOptions;
use std::io::{Error as IoError, ErrorKind};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info};
use url::Url;

const TOKEN_KEYRING_SERVICE: &str = "aws-google-oidc";
const CREDENTIALS_KEYRING_SERVICE: &str = "aws-google-oidc-sts";
const CACHE_EXPIRY_MARGIN_SECS: u64 = 60;

#[derive(Parser)]
#[command(
    version,
    about = "Exchange a Google identity token for temporary AWS credentials"
)]
struct Cli {
    /// Name used to keep cached entries separate and identify status messages.
    #[arg(long)]
    profile: String,
    /// AWS region in which to call STS.
    #[arg(long)]
    region: String,
    /// IAM role to assume with the Google identity token.
    #[arg(long)]
    role_arn: String,
    /// Google service account to impersonate.
    #[arg(long, value_name = "EMAIL")]
    impersonate_service_account: String,
    /// Audience included in the Google identity token.
    #[arg(long)]
    audience: String,
    /// Override the AWS STS endpoint.
    #[arg(long)]
    sts_endpoint_url: Option<Url>,
    /// Path to log file. Since calling process captures stdout,
    /// logging is only enabled when this is option is passed.
    #[arg(long)]
    log_file: Option<PathBuf>,
    /// Verbosity level
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Deserialize)]
struct TokenClaims {
    exp: u64,
    aud: String,
    email: String,
}

#[derive(Deserialize, Serialize)]
struct CachedToken {
    token: String,
    expires_at: u64,
    impersonate: String,
    audience: String,
}

#[derive(Deserialize, Serialize)]
struct CachedCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: String,
    expiration: String,
    expires_at: u64,
    role_arn: String,
    impersonate: String,
    audience: String,
    region: String,
    sts_endpoint_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct CredentialProcessOutput<'a> {
    version: u8,
    access_key_id: &'a str,
    secret_access_key: &'a str,
    session_token: &'a str,
    expiration: &'a str,
}

fn gcloud_error(stderr: &str) -> IoError {
    if stderr.contains("Reauthentication failed")
        || stderr.contains("cannot prompt during non-interactive execution")
    {
        return IoError::other("Google Cloud login expired; run: gcloud auth login");
    }

    let detail = stderr
        .lines()
        .find_map(|line| line.split_once("ERROR:").map(|(_, detail)| detail.trim()))
        .or_else(|| stderr.lines().rev().find(|line| !line.trim().is_empty()))
        .unwrap_or("unknown error");
    IoError::other(format!("gcloud failed: {detail}"))
}

fn error_chain(error: &dyn Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    message
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs()
}

impl CachedToken {
    fn new(token: String, cli: &Cli) -> Result<Self, Box<dyn Error>> {
        let payload = token.split('.').nth(1).ok_or_else(|| {
            IoError::new(ErrorKind::InvalidData, "gcloud returned an invalid JWT")
        })?;
        let claims: TokenClaims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;

        if claims.aud != cli.audience {
            return Err(
                IoError::new(ErrorKind::InvalidData, "token audience does not match").into(),
            );
        }
        if claims.email != cli.impersonate_service_account {
            return Err(IoError::new(
                ErrorKind::InvalidData,
                "token service account does not match",
            )
            .into());
        }

        Ok(Self {
            token,
            expires_at: claims.exp,
            impersonate: claims.email,
            audience: claims.aud,
        })
    }

    fn is_valid_for(&self, cli: &Cli) -> bool {
        self.expires_at > unix_timestamp() + CACHE_EXPIRY_MARGIN_SECS
            && self.impersonate == cli.impersonate_service_account
            && self.audience == cli.audience
    }
}

impl CachedCredentials {
    fn is_valid_for(&self, cli: &Cli) -> bool {
        self.expires_at > unix_timestamp() + CACHE_EXPIRY_MARGIN_SECS
            && self.role_arn == cli.role_arn
            && self.impersonate == cli.impersonate_service_account
            && self.audience == cli.audience
            && self.region == cli.region
            && self.sts_endpoint_url.as_deref() == cli.sts_endpoint_url.as_ref().map(Url::as_str)
    }
}

impl Cli {
    async fn sdk_config(&self) -> aws_config::SdkConfig {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(Region::new(self.region.clone()))
            .no_credentials();
        let config = match &self.sts_endpoint_url {
            Some(endpoint) => config.endpoint_url(endpoint.to_string()),
            None => config,
        };
        config.load().await
    }

    fn token(&self) -> Result<CachedToken, Box<dyn Error>> {
        let entry = keyring::Entry::new(TOKEN_KEYRING_SERVICE, &self.profile)?;

        match entry.get_secret() {
            Ok(secret) => {
                if let Ok(token) = serde_json::from_slice::<CachedToken>(&secret)
                    && token.is_valid_for(self)
                {
                    debug!("Found cached JWT token");
                    return Ok(token);
                }
            }
            Err(keyring::Error::NoEntry) => {
                debug!("Cached token not found, fetching from gcloud");
            }
            Err(error) => return Err(Box::new(error)),
        }

        let output = Command::new("gcloud")
            .args(["auth", "print-identity-token", "--quiet", "--include-email"])
            .arg(format!(
                "--impersonate-service-account={}",
                self.impersonate_service_account
            ))
            .arg(format!("--audiences={}", self.audience))
            .output()?;
        if !output.status.success() {
            return Err(gcloud_error(&String::from_utf8_lossy(&output.stderr)).into());
        }

        let token = String::from_utf8(output.stdout)?.trim().to_owned();
        let token = CachedToken::new(token, self)?;
        entry.set_secret(&serde_json::to_vec(&token)?)?;
        Ok(token)
    }

    async fn credentials(&self) -> Result<CachedCredentials, Box<dyn Error>> {
        let entry = keyring::Entry::new(CREDENTIALS_KEYRING_SERVICE, &self.profile)?;

        match entry.get_secret() {
            Ok(secret) => {
                if let Ok(credentials) = serde_json::from_slice::<CachedCredentials>(&secret)
                    && credentials.is_valid_for(self)
                {
                    debug!("Found cached STS credentials");
                    return Ok(credentials);
                }
                debug!("Cached STS credentials are invalid or expired");
            }
            Err(keyring::Error::NoEntry) => {
                debug!("Cached STS credentials not found");
            }
            Err(error) => return Err(Box::new(error)),
        }

        info!("Fetching JWT token");
        let token = self.token()?;
        debug!("JWT token fetched");

        let sdk_config = self.sdk_config().await;
        let client = aws_sdk_sts::Client::new(&sdk_config);
        debug!("Constructed STS client");

        let response = client
            .assume_role_with_web_identity()
            .role_arn(&self.role_arn)
            .role_session_name("aws-google-oidc")
            .web_identity_token(token.token)
            .send()
            .await?;
        info!("Received STS response");

        let credentials = response.credentials().ok_or_else(|| {
            IoError::new(
                ErrorKind::InvalidData,
                "STS response did not contain credentials",
            )
        })?;
        let expiration = SystemTime::try_from(*credentials.expiration())?;
        let cached = CachedCredentials {
            access_key_id: credentials.access_key_id().to_owned(),
            secret_access_key: credentials.secret_access_key().to_owned(),
            session_token: credentials.session_token().to_owned(),
            expiration: credentials.expiration().to_string(),
            expires_at: expiration.duration_since(UNIX_EPOCH)?.as_secs(),
            role_arn: self.role_arn.clone(),
            impersonate: self.impersonate_service_account.clone(),
            audience: self.audience.clone(),
            region: self.region.clone(),
            sts_endpoint_url: self.sts_endpoint_url.as_ref().map(Url::to_string),
        };
        entry.set_secret(&serde_json::to_vec(&cached)?)?;
        Ok(cached)
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    if let Some(logfilepath) = &cli.log_file {
        let logfile = OpenOptions::new()
            .create(true)
            .append(true)
            .open(logfilepath)?;
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(Mutex::new(logfile))
            .with_max_level(if cli.verbose {
                tracing::Level::DEBUG
            } else {
                tracing::Level::INFO
            })
            .init();
    }
    let credentials = cli.credentials().await?;
    let output = CredentialProcessOutput {
        version: 1,
        access_key_id: &credentials.access_key_id,
        secret_access_key: &credentials.secret_access_key,
        session_token: &credentials.session_token,
        expiration: &credentials.expiration,
    };
    let expiration: chrono::DateTime<chrono::Local> = SystemTime::UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(credentials.expires_at))
        .ok_or_else(|| IoError::new(ErrorKind::InvalidData, "invalid credential expiration"))?
        .into();

    info!(
        profile = %cli.profile,
        expiration = %expiration.format("%Y-%m-%d %H:%M:%S %Z (%:z)"),
        "using credentials"
    );

    serde_json::to_writer(std::io::stdout(), &output)?;
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = error_chain(error.as_ref());
            tracing::error!(error = ?error, "{message}");
            eprintln!("aws-google-oidc: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CachedCredentials, Cli, error_chain, gcloud_error};
    use clap::Parser;
    use std::error::Error;
    use std::fmt::{self, Display};

    #[derive(Debug)]
    struct InnerError;

    impl Display for InnerError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("inner error")
        }
    }

    impl Error for InnerError {}

    #[derive(Debug)]
    struct OuterError;

    impl Display for OuterError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("outer error")
        }
    }

    impl Error for OuterError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&InnerError)
        }
    }

    fn example_cli() -> Cli {
        Cli::try_parse_from([
            "aws-google-oidc",
            "--profile",
            "example",
            "--region",
            "eu-central-1",
            "--role-arn",
            "arn:aws:iam::123456789012:role/google-oidc",
            "--impersonate-service-account",
            "aws-users@example.iam.gserviceaccount.com",
            "--audience",
            "aws-google-oidc",
        ])
        .expect("credential-process flags should parse")
    }

    fn cached_credentials(cli: &Cli, expires_at: u64) -> CachedCredentials {
        CachedCredentials {
            access_key_id: "access-key".into(),
            secret_access_key: "secret-key".into(),
            session_token: "session-token".into(),
            expiration: "expiration".into(),
            expires_at,
            role_arn: cli.role_arn.clone(),
            impersonate: cli.impersonate_service_account.clone(),
            audience: cli.audience.clone(),
            region: cli.region.clone(),
            sts_endpoint_url: None,
        }
    }

    #[test]
    fn parses_credential_process_flags() {
        let cli = example_cli();

        assert_eq!(cli.profile, "example");
        assert_eq!(cli.region, "eu-central-1");
        assert_eq!(cli.audience, "aws-google-oidc");
        assert!(cli.sts_endpoint_url.is_none());
    }

    #[test]
    fn validates_cached_credentials_against_sts_inputs() {
        let mut cli = example_cli();
        let credentials = cached_credentials(&cli, u64::MAX);

        assert!(credentials.is_valid_for(&cli));

        cli.role_arn = "arn:aws:iam::123456789012:role/other".into();
        assert!(!credentials.is_valid_for(&cli));
    }

    #[test]
    fn rejects_expired_cached_credentials() {
        let cli = example_cli();
        let credentials = cached_credentials(&cli, 0);

        assert!(!credentials.is_valid_for(&cli));
    }

    #[test]
    fn makes_reauthentication_errors_actionable() {
        let stderr = "WARNING: using impersonation\nERROR: Reauthentication failed. cannot prompt during non-interactive execution.\nPlease run gcloud auth login";

        assert_eq!(
            gcloud_error(stderr).to_string(),
            "Google Cloud login expired; run: gcloud auth login"
        );
    }

    #[test]
    fn omits_warnings_from_other_gcloud_errors() {
        let stderr = "WARNING: using impersonation\nERROR: permission denied\nmore details";

        assert_eq!(
            gcloud_error(stderr).to_string(),
            "gcloud failed: permission denied"
        );
    }

    #[test]
    fn displays_the_full_error_chain() {
        assert_eq!(error_chain(&OuterError), "outer error: inner error");
    }
}
