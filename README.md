# aws-google-oidc

[![CI](https://github.com/dialohq/aws-google-oidc/actions/workflows/ci.yml/badge.svg)](https://github.com/dialohq/aws-google-oidc/actions/workflows/ci.yml)
[![Release](https://github.com/dialohq/aws-google-oidc/actions/workflows/release.yml/badge.svg)](https://github.com/dialohq/aws-google-oidc/releases)

Use your existing Google login as an AWS `credential_process` with AWS STS or compatible services such as Ceph, without permanent AWS access keys and with secure credential caching in your operating system keychain.

`aws-google-oidc` uses `gcloud` to impersonate a Google service account, obtains an OIDC identity token, and exchanges it for temporary credentials through AWS STS or a configured compatible endpoint. AWS tools invoke it automatically, while reusable Google tokens and STS credentials remain in the system credential store until shortly before they expire.

## Install

The install script downloads the latest checksummed binary to `~/.local/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://dialohq.github.io/aws-google-oidc/install.sh | sh
```

Prebuilt releases are available for x86_64 Linux, aarch64 Linux, and Apple Silicon macOS on the [Releases page](https://github.com/dialohq/aws-google-oidc/releases). The Linux binaries are static.

### Nix

The project is available as the `github:dialohq/aws-google-oidc` flake. Prebuilt outputs are available from the public [aws-google-oidc Cachix cache](https://app.cachix.org/cache/aws-google-oidc).

## Configure the role

The IAM role must trust Google as an OIDC provider. Get the OAuth client ID of the service account you want to impersonate:

```sh
gcloud iam service-accounts describe \
  aws-users@example.iam.gserviceaccount.com \
  --format='value(oauth2ClientId)'
```

Use it in the role's trust policy:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Principal": { "Federated": "accounts.google.com" },
      "Action": "sts:AssumeRoleWithWebIdentity",
      "Condition": {
        "StringEquals": {
          "accounts.google.com:aud": "SERVICE_ACCOUNT_OAUTH2_CLIENT_ID",
          "accounts.google.com:oaud": "aws-google-oidc",
          "accounts.google.com:email": "aws-users@example.iam.gserviceaccount.com"
        }
      }
    }
  ]
}
```

AWS maps `aud` to the Google token's authorized-party claim and `oaud` to its requested audience. The `email` condition restricts access to the intended service account. See the [AWS OIDC condition-key reference](https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_policies_iam-condition-keys.html#condition-keys-wif).

## Configure AWS

Add a profile to `~/.aws/config`:

```ini
[profile google-oidc]
region = eu-central-1
credential_process = aws-google-oidc
  --profile google-oidc
  --region eu-central-1
  --role-arn arn:aws:iam::123456789012:role/google-oidc
  --impersonate-service-account aws-users@example.iam.gserviceaccount.com
  --audience aws-google-oidc
```

The helper-specific settings are flags on `credential_process`; the profile itself only contains normal AWS configuration. Add `--sts-endpoint-url URL` when connecting to Ceph Object Gateway or another STS-compatible endpoint.

## Use

Log into Google, then use the AWS profile normally:

```sh
gcloud auth login
aws sts get-caller-identity --profile google-oidc
```

The AWS CLI invokes `aws-google-oidc` automatically. Your Google identity must be allowed to impersonate the configured service account.

## Development

```sh
nix develop
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## License

[MIT](LICENSE)
