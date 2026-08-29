# aws-google-oidc

[![CI](https://github.com/dialohq/aws-google-oidc/actions/workflows/ci.yml/badge.svg)](https://github.com/dialohq/aws-google-oidc/actions/workflows/ci.yml)
[![Release](https://github.com/dialohq/aws-google-oidc/actions/workflows/release.yml/badge.svg)](https://github.com/dialohq/aws-google-oidc/releases)

`aws-google-oidc` is an [AWS `credential_process`](https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-sourcing-external.html) that exchanges a Google identity token for temporary AWS credentials through `AssumeRoleWithWebIdentity`.

It uses your existing `gcloud` login to impersonate a Google service account, asks Google for an identity token, and exchanges that token with AWS STS. AWS tools invoke the process automatically when you select the configured profile; the Google token is cached in the operating system keyring until shortly before it expires.

AWS STS can [validate Google-specific identity claims](https://aws.amazon.com/about-aws/whats-new/2026/01/aws-sts-supports-validation-identity-provider-claims/) such as `email` in an IAM role trust policy. This lets the role trust one particular impersonated Google service account instead of a long-lived AWS access key.

## Install

### Install script

The installer downloads a checksummed binary from the latest GitHub Release into `~/.local/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://dialohq.github.io/aws-google-oidc/install.sh | sh
```

To select a release or installation directory:

```sh
AWS_GOOGLE_OIDC_VERSION=v0.1.0 \
AWS_GOOGLE_OIDC_INSTALL_DIR="$HOME/bin" \
sh -c "$(curl --proto '=https' --tlsv1.2 -LsSf https://dialohq.github.io/aws-google-oidc/install.sh)"
```

Prebuilt releases support static Linux binaries for x86_64 and aarch64, plus Apple Silicon macOS. The archives and SHA-256 checksums are available from the [Releases page](https://github.com/dialohq/aws-google-oidc/releases).

### Nix

CI publishes all supported systems to the public `aws-google-oidc` Cachix cache. Enable it before installing:

```sh
nix run nixpkgs#cachix -- use aws-google-oidc
nix profile install github:dialohq/aws-google-oidc
```

The cache is public at [app.cachix.org/cache/aws-google-oidc](https://app.cachix.org/cache/aws-google-oidc). Point `credential_process` at the executable provided by your chosen Nix setup; its location depends on how you integrate the flake.

For a declarative setup, add the flake input:

```nix
{
  inputs.aws-google-oidc.url = "github:dialohq/aws-google-oidc";
}
```

If Nix generates your AWS config, reference the package directly in the profile attribute set. This keeps the executable in the system closure and gives `credential_process` an immutable path:

```nix
let
  awsGoogleOidc = aws-google-oidc.packages.${system}.default;
in {
  "profile google-oidc" = {
    region = "eu-central-1";
    credential_process = "${awsGoogleOidc}/bin/aws-google-oidc --profile google-oidc --region eu-central-1 --role-arn arn:aws:iam::123456789012:role/google-oidc --impersonate-service-account aws-users@example.iam.gserviceaccount.com --audience aws-google-oidc";
  };
}
```

## Configure AWS

### 1. Trust the Google identity

First get the service account's OAuth 2.0 client ID, which Google puts in the token's `azp` claim:

```sh
gcloud iam service-accounts describe \
  aws-users@example.iam.gserviceaccount.com \
  --format='value(oauth2ClientId)'
```

Then configure the IAM role trust policy for Google OIDC. Restrict the authorized party, token audience, and service-account email:

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

Google service-account tokens include an `azp` claim, so AWS maps `accounts.google.com:aud` to the service account's OAuth client ID and `accounts.google.com:oaud` to the audience requested by this tool. See AWS's [Google workload identity guide](https://aws.amazon.com/blogs/security/access-aws-using-a-google-cloud-platform-native-workload-identity/) and [OIDC condition-key reference](https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_policies_iam-condition-keys.html#condition-keys-wif) for the mapping.

### 2. Add an AWS profile

Add the process to `~/.aws/config`. All helper-specific values are command-line flags; the surrounding profile contains only ordinary AWS settings:

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

Use `--sts-endpoint-url URL` only when targeting a custom STS-compatible endpoint. Without it, the AWS SDK uses the regional AWS STS endpoint.

The `--profile` value is a cache namespace and appears in status messages. It should normally match the AWS profile name.

### 3. Authenticate and test

Authenticate interactively once, then let the AWS CLI invoke the process:

```sh
gcloud auth login
aws sts get-caller-identity --profile google-oidc
```

Your Google identity needs permission to impersonate the configured service account. If the login expires, `aws-google-oidc` asks you to run `gcloud auth login` again without dumping the full `gcloud` error into the AWS CLI output.

## Develop

```sh
nix develop
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Every pull request builds the Nix package and portable release artifact on all supported platforms. Pushing a tag matching the Cargo package version, such as `v0.1.0`, publishes those artifacts as a GitHub Release.
