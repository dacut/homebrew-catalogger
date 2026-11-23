#!/usr/bin/env bash
#
# Builds the website-update Lambda function for AWS Lambda (Arm64, provided.al2023 runtime) and packages it as
# target/lambda/homebrew-catalog-lambda/bootstrap.zip, which ./terraform deploys when enable_automation is true.
#
# Requires cargo-lambda (https://www.cargo-lambda.info/) and cross (https://github.com/cross-rs/cross), which builds
# inside a container, so Docker (or Podman, with CROSS_CONTAINER_ENGINE=podman) must be running. cross is used rather
# than cargo-lambda's default Zig linker, which warns about rustc's -Wl,-O1 on every release build.

set -euo pipefail

die() {
    echo "build-lambda: $*" >&2
    exit 1
}

main() {
    repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
    cd "$repo_root"

    command -v cargo-lambda >/dev/null ||
        die "cargo-lambda is not installed; see https://www.cargo-lambda.info/guide/installation.html"
    command -v cross >/dev/null ||
        die "cross is not installed; run: cargo install cross --git https://github.com/cross-rs/cross"

    engine=${CROSS_CONTAINER_ENGINE:-docker}
    command -v "$engine" >/dev/null || die "$engine is not installed (cross builds inside a container)"
    "$engine" info >/dev/null 2>&1 || die "$engine isn't running; start it and try again (cross builds inside a container)"

    cargo lambda build --release --compiler cross --arm64 --output-format zip -p homebrew-catalog-lambda
    echo "Built $repo_root/target/lambda/homebrew-catalog-lambda/bootstrap.zip"
}

# Everything runs inside main, which bash reads in full before running any of it, so editing this file while it runs
# can't change what runs. The exit on the same line stops bash from reading anything after it.
main "$@"; exit
