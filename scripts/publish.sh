#!/usr/bin/env bash
#
# Regenerates the Homebrew formula catalog site and publishes it to the S3 bucket and CloudFront distribution
# created by ./terraform:
#
#   1. Builds the site generator in release mode and regenerates ./site from scratch, so a stale build or leftover
#      files can never be published.
#   2. Uploads stylesheets first. Their names are content-hashed, so they're cached for a year and old ones are kept:
#      pages cached elsewhere may still reference them.
#   3. Uploads the catalog (the input file) to the bucket root as formula-catalogged.json, where the scheduled update
#      function (terraform's enable_automation) reads it.
#   4. Uploads pages, cached for 5 minutes, deleting pages that no longer exist.
#   5. Invalidates the CloudFront cache.
#
# If the update function is enabled, it may have classified formulae since your local catalog was made. Download its
# catalog first (see the catalog_url Terraform output) so that publishing doesn't discard that work.
#
# AWS credentials come from the usual AWS CLI configuration (AWS_PROFILE, environment variables, SSO, ...).

set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/publish.sh [options]

Regenerates the site and publishes it to the bucket and CloudFront distribution in ./terraform's outputs.

Options:
  -i, --input FILE   Classified formula metadata (default: formula-catalogged.json)
  -n, --dry-run      Generate the site and show what would be uploaded or deleted, without changing anything
  -w, --wait         Wait for the CloudFront invalidation to finish
  -y, --yes          Publish without asking for confirmation
  -h, --help         Show this help
EOF
}

die() {
    echo "publish: $*" >&2
    exit 1
}

tf_output() {
    terraform -chdir=terraform output -raw "$1" 2>/dev/null ||
        die "couldn't read Terraform output \"$1\"; run \"terraform apply\" in ./terraform first"
}

main() {
    repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
    cd "$repo_root"

    input=formula-catalogged.json
    dry_run=false
    wait_for_invalidation=false
    assume_yes=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
        -i | --input)
            [[ $# -ge 2 ]] || die "$1 requires a file name"
            input=$2
            shift 2
            ;;
        -n | --dry-run)
            dry_run=true
            shift
            ;;
        -w | --wait)
            wait_for_invalidation=true
            shift
            ;;
        -y | --yes)
            assume_yes=true
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
        esac
    done

    for tool in cargo terraform aws; do
        command -v "$tool" >/dev/null || die "$tool is not installed or not on PATH"
    done
    [[ -f $input ]] || die "$input not found; run homebrew-catalog-classifier first, or pass --input"

    # Read the deployment targets before doing any work, so a missing `terraform apply` fails fast.
    bucket=$(tf_output bucket_name)
    distribution_id=$(tf_output distribution_id)
    site_url=$(tf_output site_url)

    site_dir=site
    echo "==> Generating $site_dir from $input"
    rm -rf -- "$site_dir"
    catalog_key=formula-catalogged.json
    cargo run --quiet --release -p homebrew-catalog-static-pages -- "$input" --output-dir "$site_dir" --base-url "$site_url" \
        --catalog-path "$catalog_key"

    echo
    echo "    Bucket:       s3://$bucket"
    echo "    Distribution: $distribution_id"
    echo "    Site:         $site_url"
    echo

    if ! $dry_run && ! $assume_yes; then
        [[ -t 0 ]] || die "not running interactively; pass --yes to publish without confirmation"
        read -r -p "Publish to $site_url? [y/N] " answer
        [[ $answer == [yY]* ]] || die "cancelled"
    fi

    sync_options=(--no-progress)
    $dry_run && sync_options+=(--dryrun)

    echo "==> Uploading stylesheets"
    aws s3 sync "$site_dir/assets" "s3://$bucket/assets/" \
        --cache-control "public, max-age=31536000, immutable" "${sync_options[@]}"

    echo "==> Uploading the catalog"
    if $dry_run; then
        echo "(dryrun) upload: $input to s3://$bucket/$catalog_key"
    else
        aws s3 cp "$input" "s3://$bucket/$catalog_key" --no-progress \
            --content-type application/json --cache-control "public, max-age=300"
    fi

    echo "==> Uploading pages"
    aws s3 sync "$site_dir" "s3://$bucket/" \
        --delete --exclude "assets/*" --exclude "$catalog_key" --cache-control "public, max-age=300" "${sync_options[@]}"

    if $dry_run; then
        echo "==> Dry run: would invalidate the CloudFront cache for /*"
        exit 0
    fi

    echo "==> Invalidating the CloudFront cache"
    invalidation_id=$(aws cloudfront create-invalidation --distribution-id "$distribution_id" --paths "/*" \
        --query Invalidation.Id --output text)

    if $wait_for_invalidation; then
        echo "    Waiting for invalidation $invalidation_id to finish..."
        aws cloudfront wait invalidation-completed --distribution-id "$distribution_id" --id "$invalidation_id"
        echo "==> Published to $site_url"
    else
        echo "==> Published. Invalidation $invalidation_id is in progress; changes appear at $site_url within a few minutes."
    fi
}

# Everything runs inside main, which bash reads in full before running any of it, so editing this file while it runs
# can't change what runs. The exit on the same line stops bash from reading anything after it.
main "$@"; exit
