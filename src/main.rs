use aws_sdk_sts::config::Region;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::io::{Error as IoError, ErrorKind};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

#[derive(Parser)]
#[command(about = "Exchange a Google identity token for temporary AWS credentials")]
struct Cli {
    /// Name used to keep cached tokens separate and identify status messages.
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

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct CredentialProcessOutput<'a> {
    version: u8,
    access_key_id: &'a str,
    secret_access_key: &'a str,
    session_token: &'a str,
    expiration: String,
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the Unix epoch")
            .as_secs();

        self.expires_at > now + 60
            && self.impersonate == cli.impersonate_service_account
            && self.audience == cli.audience
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
        let entry = keyring::Entry::new("aws-google-oidc", &self.profile)?;

        match entry.get_secret() {
            Ok(secret) => {
                if let Ok(token) = serde_json::from_slice::<CachedToken>(&secret)
                    && token.is_valid_for(self)
                {
                    return Ok(token);
                }
            }
            Err(keyring::Error::NoEntry) => {}
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
}

async fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let token = cli.token()?;

    let sdk_config = cli.sdk_config().await;
    let client = aws_sdk_sts::Client::new(&sdk_config);
    let response = client
        .assume_role_with_web_identity()
        .role_arn(&cli.role_arn)
        .role_session_name("aws-google-oidc")
        .web_identity_token(token.token)
        .send()
        .await?;
    let credentials = response.credentials().ok_or_else(|| {
        IoError::new(
            ErrorKind::InvalidData,
            "STS response did not contain credentials",
        )
    })?;
    let output = CredentialProcessOutput {
        version: 1,
        access_key_id: credentials.access_key_id(),
        secret_access_key: credentials.secret_access_key(),
        session_token: credentials.session_token(),
        expiration: credentials.expiration().to_string(),
    };
    let expiration: chrono::DateTime<chrono::Local> =
        SystemTime::try_from(*credentials.expiration())?.into();

    eprintln!(
        "Refreshed credentials for profile `{}`; valid until {}",
        cli.profile,
        expiration.format("%Y-%m-%d %H:%M:%S %Z (%:z)")
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
            eprintln!("aws-google-oidc: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, gcloud_error};
    use clap::Parser;

    #[test]
    fn parses_credential_process_flags() {
        let cli = Cli::try_parse_from([
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
        .expect("credential-process flags should parse");

        assert_eq!(cli.profile, "example");
        assert_eq!(cli.region, "eu-central-1");
        assert_eq!(cli.audience, "aws-google-oidc");
        assert!(cli.sts_endpoint_url.is_none());
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
}
