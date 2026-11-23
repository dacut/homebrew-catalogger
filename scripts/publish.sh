#!/usr/bin/env bash
#
# Regenerates the Homebrew formula catalog site and publishes it to the S3 bucket and CloudFront distribution
# created by ./terraform:
#
#   1. Builds the site generator in release mode and regenerates ./site from scratch, so a stale build or leftover
#      files can never be published.
#   2. Takes the publishing lock shared with the scheduled update function (terraform's enable_automation), so the
#      two never write the catalog and site at the same time, and checks that the bucket's catalog isn't newer than
#      yours (which would mean the function has classified formulae since your copy was made).
#   3. Uploads stylesheets first. Their names are content-hashed, so they're cached for a year and old ones are kept:
#      pages cached elsewhere may still reference them.
#   4. Uploads the catalog (the input file) to the bucket root as formula-catalogged.json, where the update function
#      reads it.
#   5. Uploads pages, cached for 4 hours, deleting pages that no longer exist.
#   6. Invalidates the CloudFront cache, and releases the lock.
#
# AWS credentials come from the usual AWS CLI configuration (AWS_PROFILE, environment variables, SSO, ...).

set -euo pipefail

# The lock object shared with the update function (crates/homebrew-catalog-lambda/src/lock.rs uses the same key and
# format), and how long this script holds it at most.
readonly LOCK_KEY=.publish.lock
readonly LOCK_TTL_SECONDS=1800
readonly CATALOG_KEY=formula-catalogged.json

# Cache lifetimes: a year for content-hashed stylesheets, 4 hours for everything else (pages and the catalog).
readonly ASSET_CACHE_CONTROL="public, max-age=31536000, immutable"
readonly PAGE_CACHE_CONTROL="public, max-age=14400"

usage() {
    cat <<'EOF'
Usage: scripts/publish.sh [options]

Regenerates the site and publishes it to the bucket and CloudFront distribution in ./terraform's outputs.

Options:
  -i, --input FILE   Classified formula metadata (default: formula-catalogged.json)
  -n, --dry-run      Generate the site and show what would be uploaded or deleted, without changing anything
  -f, --force        Publish even if the bucket's catalog is newer than the input (replacing it)
  -w, --wait         Wait for the CloudFront invalidation to finish
  -y, --yes          Publish without asking for confirmation
  -h, --help         Show this help
EOF
}

die() {
    echo "publish: $*" >&2
    exit 1
}

warn() {
    echo "publish: warning: $*" >&2
}

tf_output() {
    terraform -chdir=terraform output -raw "$1" 2>/dev/null ||
        die "couldn't read Terraform output \"$1\"; run \"terraform apply\" in ./terraform first"
}

# Seconds since the epoch for an S3 timestamp such as 2026-11-22T23:10:00+00:00 (always UTC), with GNU or BSD date.
iso_to_epoch() {
    local ts=${1%+00:00}
    ts=${ts%Z}
    date -u -d "$ts" +%s 2>/dev/null || TZ=UTC date -j -f '%Y-%m-%dT%H:%M:%S' "$ts" +%s 2>/dev/null
}

# A file's modification time in seconds since the epoch, with GNU or BSD stat.
file_mtime() {
    stat -c %Y "$1" 2>/dev/null || stat -f %m "$1"
}

# Takes the publishing lock (sets lock_etag), or exits if someone else holds an unexpired one. Uses the globals
# bucket and work.
acquire_lock() {
    local expires existing_etag held_until holder attempt
    expires=$(($(date +%s) + LOCK_TTL_SECONDS))
    printf '{"holder":"publish.sh","expires":%d}' "$expires" >"$work/lock.json"

    for attempt in 1 2; do
        # Create the lock only if it doesn't exist.
        if lock_etag=$(aws s3api put-object --bucket "$bucket" --key "$LOCK_KEY" --body "$work/lock.json" \
            --content-type application/json --if-none-match '*' --query ETag --output text 2>"$work/lock.err"); then
            return 0
        fi
        lock_etag=
        grep -q -e PreconditionFailed -e ConditionalRequestConflict "$work/lock.err" ||
            die "couldn't create the publishing lock: $(cat "$work/lock.err")"

        # It exists: read it, and take it over only if it has expired (e.g. left by a crash).
        existing_etag=$(aws s3api get-object --bucket "$bucket" --key "$LOCK_KEY" "$work/held.json" \
            --query ETag --output text 2>/dev/null) || continue # Released in the meantime; try again.
        held_until=$(sed -n 's/.*"expires":\([0-9][0-9]*\).*/\1/p' "$work/held.json")
        holder=$(sed -n 's/.*"holder":"\([^"]*\)".*/\1/p' "$work/held.json")
        if [[ -n $held_until ]] && ((held_until > $(date +%s))); then
            die "the site is being updated by ${holder:-someone else} (for up to $(((held_until - $(date +%s) + 59) / 60)) more minutes); try again later"
        fi
        if lock_etag=$(aws s3api put-object --bucket "$bucket" --key "$LOCK_KEY" --body "$work/lock.json" \
            --content-type application/json --if-match "$existing_etag" --query ETag --output text 2>/dev/null); then
            echo "    (took over an expired lock held by ${holder:-someone else})"
            return 0
        fi
        lock_etag=
    done
    die "the publishing lock kept changing; another update is probably starting. Try again in a few minutes."
}

# Releases the lock if this script holds it, and only if it's still ours (not taken over after expiring).
release_lock() {
    [[ -n ${lock_etag:-} ]] || return 0
    aws s3api delete-object --bucket "$bucket" --key "$LOCK_KEY" --if-match "$lock_etag" >/dev/null 2>&1 ||
        warn "couldn't release the publishing lock; it expires on its own within $((LOCK_TTL_SECONDS / 60)) minutes"
    lock_etag=
}

# Succeeds if publishing the input wouldn't discard newer work in the bucket: there's no catalog there, it's
# identical to the input, or the input is newer. Uses the globals bucket, input, and work.
catalog_is_current() {
    local modified
    if ! modified=$(aws s3api head-object --bucket "$bucket" --key "$CATALOG_KEY" --query LastModified --output text \
        2>"$work/head.err"); then
        grep -q -e 'Not Found' -e 404 "$work/head.err" && return 0 # No catalog in the bucket yet.
        die "couldn't check the bucket's catalog: $(cat "$work/head.err")"
    fi
    aws s3 cp --quiet "s3://$bucket/$CATALOG_KEY" "$work/bucket-catalog.json"
    cmp -s "$input" "$work/bucket-catalog.json" && return 0

    local bucket_epoch
    bucket_epoch=$(iso_to_epoch "$modified") || {
        warn "couldn't interpret the bucket catalog's date ($modified); assuming it's newer"
        return 1
    }
    ((bucket_epoch <= $(file_mtime "$input")))
}

main() {
    repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
    cd "$repo_root"

    input=formula-catalogged.json
    dry_run=false
    force=false
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
        -f | --force)
            force=true
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

    # Scratch space, and the lock's release, are cleaned up however the script exits. (Globals, so the trap still
    # sees them after main returns.)
    work=$(mktemp -d)
    lock_etag=
    trap 'release_lock; rm -rf "$work"' EXIT

    # Read the deployment targets before doing any work, so a missing `terraform apply` fails fast.
    bucket=$(tf_output bucket_name)
    distribution_id=$(tf_output distribution_id)
    site_url=$(tf_output site_url)

    site_dir=site
    echo "==> Generating $site_dir from $input"
    rm -rf -- "$site_dir"
    cargo run --quiet --release -p homebrew-catalog-static-pages -- "$input" --output-dir "$site_dir" --base-url "$site_url" \
        --catalog-path "$CATALOG_KEY"

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

    local stale_message="the bucket's catalog is newer than $input and differs from it: the update function has \
probably classified formulae since your copy was made. Download it with
    aws s3 cp s3://$bucket/$CATALOG_KEY $input
and publish again, or pass --force to replace it anyway."
    if $dry_run; then
        catalog_is_current || warn "$stale_message"
    else
        # Held from here until the script exits, so the update function can't change the catalog or site in between.
        echo "==> Taking the publishing lock"
        acquire_lock
        if ! $force && ! catalog_is_current; then
            die "$stale_message"
        fi
    fi

    sync_options=(--no-progress)
    $dry_run && sync_options+=(--dryrun)

    echo "==> Uploading stylesheets"
    aws s3 sync "$site_dir/assets" "s3://$bucket/assets/" \
        --cache-control "$ASSET_CACHE_CONTROL" "${sync_options[@]}"

    echo "==> Uploading the catalog"
    if $dry_run; then
        echo "(dryrun) upload: $input to s3://$bucket/$CATALOG_KEY"
    else
        aws s3 cp "$input" "s3://$bucket/$CATALOG_KEY" --no-progress \
            --content-type application/json --cache-control "$PAGE_CACHE_CONTROL"
    fi

    echo "==> Uploading pages"
    aws s3 sync "$site_dir" "s3://$bucket/" --delete --exclude "assets/*" --exclude "$CATALOG_KEY" \
        --exclude "$LOCK_KEY" --cache-control "$PAGE_CACHE_CONTROL" "${sync_options[@]}"

    if $dry_run; then
        echo "==> Dry run: would invalidate the CloudFront cache for /*"
        exit 0
    fi

    echo "==> Invalidating the CloudFront cache"
    invalidation_id=$(aws cloudfront create-invalidation --distribution-id "$distribution_id" --paths "/*" \
        --query Invalidation.Id --output text)
    release_lock

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
