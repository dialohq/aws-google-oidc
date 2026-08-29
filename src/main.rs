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
struct Cli {
    #[arg(long)]
    profile: String,
}

struct DialoProfile {
    dialo_role_arn: String,
    dialo_gcloud_impersonate: String,
    dialo_gcloud_token_audience: String,
}

struct Profile {
    dialo: DialoProfile,
    region: Region,
    endpoint: Url,
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
    fn new(token: String, profile: &DialoProfile) -> Result<Self, Box<dyn Error>> {
        let payload = token.split('.').nth(1).ok_or_else(|| {
            IoError::new(ErrorKind::InvalidData, "gcloud returned an invalid JWT")
        })?;
        let claims: TokenClaims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;

        if claims.aud != profile.dialo_gcloud_token_audience {
            return Err(
                IoError::new(ErrorKind::InvalidData, "token audience does not match").into(),
            );
        }
        if claims.email != profile.dialo_gcloud_impersonate {
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

    fn is_valid_for(&self, profile: &DialoProfile) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the Unix epoch")
            .as_secs();

        self.expires_at > now + 60
            && self.impersonate == profile.dialo_gcloud_impersonate
            && self.audience == profile.dialo_gcloud_token_audience
    }
}

impl Profile {
    async fn new(profile_name: &str) -> Result<Self, Box<dyn Error>> {
        let profiles = aws_config::profile::load(
            &Default::default(),
            &Default::default(),
            &Default::default(),
            None,
        )
        .await?;
        let profile = profiles
            .get_profile(profile_name)
            .unwrap_or_else(|| panic!("Profile {profile_name} does not exist"));
        let get = |key| {
            profile
                .get(key)
                .unwrap_or_else(|| panic!("Profile {profile_name} is missing key {key}"))
                .into()
        };

        Ok(Self {
            dialo: DialoProfile {
                dialo_role_arn: get("dialo_role_arn"),
                dialo_gcloud_impersonate: get("dialo_gcloud_impersonate"),
                dialo_gcloud_token_audience: get("dialo_gcloud_token_audience"),
            },
            region: Region::new(get("region")),
            endpoint: Url::parse(&get("endpoint_url"))?,
        })
    }

    async fn sdk_config(&self) -> aws_config::SdkConfig {
        aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(self.region.clone())
            .endpoint_url(self.endpoint.to_string())
            .no_credentials()
            .load()
            .await
    }

    fn token(&self, profile_name: &str) -> Result<CachedToken, Box<dyn Error>> {
        let entry = keyring::Entry::new("aws-google-oidc", profile_name)?;

        match entry.get_secret() {
            Ok(secret) => {
                if let Ok(token) = serde_json::from_slice::<CachedToken>(&secret)
                    && token.is_valid_for(&self.dialo)
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
                self.dialo.dialo_gcloud_impersonate
            ))
            .arg(format!(
                "--audiences={}",
                self.dialo.dialo_gcloud_token_audience
            ))
            .output()?;
        if !output.status.success() {
            return Err(gcloud_error(&String::from_utf8_lossy(&output.stderr)).into());
        }

        let token = String::from_utf8(output.stdout)?.trim().to_owned();
        let token = CachedToken::new(token, &self.dialo)?;
        entry.set_secret(&serde_json::to_vec(&token)?)?;
        Ok(token)
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let profile = Profile::new(&cli.profile).await?;
    let token = profile.token(&cli.profile)?;

    let sdk_config = profile.sdk_config().await;
    let client = aws_sdk_sts::Client::new(&sdk_config);
    let response = client
        .assume_role_with_web_identity()
        .role_arn(&profile.dialo.dialo_role_arn)
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
    use super::gcloud_error;

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
