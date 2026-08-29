# aws-google-oidc

[![CI](https://github.com/dialohq/aws-google-oidc/actions/workflows/ci.yml/badge.svg)](https://github.com/dialohq/aws-google-oidc/actions/workflows/ci.yml)
[![Release](https://github.com/dialohq/aws-google-oidc/actions/workflows/release.yml/badge.svg)](https://github.com/dialohq/aws-google-oidc/releases)

`aws-google-oidc` is an [AWS `credential_process`](https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-sourcing-external.html) that exchanges a Google identity token for short-lived AWS credentials. It is designed for S3-compatible services backed by Ceph RGW, but works with any STS endpoint that supports `AssumeRoleWithWebIdentity`.

It asks `gcloud` for an identity token using service-account impersonation, exchanges that token with STS, and emits the credential JSON expected by AWS tools. Google identity tokens are cached in the operating system keyring until shortly before they expire.

## Install

### Install script

The installer downloads a checksummed binary from the latest GitHub Release into `~/.local/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://dialohq.github.io/aws-google-oidc/install.sh | sh
```

Make sure `~/.local/bin` is on `PATH`. To select a release or installation directory:

```sh
AWS_GOOGLE_OIDC_VERSION=v0.1.0 \
AWS_GOOGLE_OIDC_INSTALL_DIR="$HOME/bin" \
sh -c "$(curl --proto '=https' --tlsv1.2 -LsSf https://dialohq.github.io/aws-google-oidc/install.sh)"
```

Prebuilt releases support:

- Linux x86_64 (static)
- Linux aarch64 (static)
- macOS Apple Silicon (aarch64)

The archives and their SHA-256 checksums are also available from the [Releases page](https://github.com/dialohq/aws-google-oidc/releases).

### Nix

Run without installing:

```sh
nix run github:dialohq/aws-google-oidc -- --profile ceph
```

Or install it into your Nix profile:

```sh
nix profile install github:dialohq/aws-google-oidc
```

CI publishes builds to the public `aws-google-oidc` Cachix cache. Enable it before running or installing to avoid rebuilding locally:

```sh
nix run nixpkgs#cachix -- use aws-google-oidc
```

To consume the package from another flake:

```nix
{
  inputs.aws-google-oidc.url = "github:dialohq/aws-google-oidc";

  outputs = {nixpkgs, aws-google-oidc, ...}: let
    system = "aarch64-darwin"; # or x86_64-linux / aarch64-linux
    pkgs = nixpkgs.legacyPackages.${system};
  in {
    devShells.${system}.default = pkgs.mkShell {
      packages = [aws-google-oidc.packages.${system}.default];
    };
  };
}
```

In Nix-generated AWS configuration, reference the package directly so `credential_process` has a stable absolute path:

```nix
credential_process = "${aws-google-oidc.packages.${system}.default}/bin/aws-google-oidc --profile ceph";
```

## Configure

Prerequisites:

- the [Google Cloud CLI](https://cloud.google.com/sdk/docs/install), authenticated with `gcloud auth login`
- permission to impersonate the configured Google service account
- an S3-compatible endpoint exposing STS `AssumeRoleWithWebIdentity`
- an available system keyring (Keychain on macOS or Secret Service on Linux)

Add a profile to `~/.aws/config`, replacing the example values and using the absolute path to the installed executable:

```ini
[profile ceph]
region = eu-central-1
endpoint_url = https://s3.example.com
credential_process = /home/alice/.local/bin/aws-google-oidc --profile ceph
s3 =
  addressing_style = path
dialo_role_arn = arn:aws:iam::RGW_ACCOUNT_ID:role/example-role
dialo_gcloud_impersonate = ceph-users@example-project.iam.gserviceaccount.com
dialo_gcloud_token_audience = ceph-rgw
```

The name passed to `--profile` must match the AWS profile containing the custom `dialo_*` settings. Test the complete flow with any AWS-compatible client, for example:

```sh
aws s3 ls --profile ceph s3://
```

If Google authentication has expired, renew it interactively and retry:

```sh
gcloud auth login
```

## Develop

Enter the development shell and run the checks:

```sh
nix develop
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Every pull request builds the Nix package and portable release artifact on all supported platforms. Pushing a tag matching the Cargo package version, such as `v0.1.0`, publishes those artifacts as a GitHub Release.
