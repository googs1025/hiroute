#!/usr/bin/env bash
set -euo pipefail

asset_dir=${1:?usage: publish-release-oss.sh ASSET_DIR MANIFEST TAG}
manifest=${2:?usage: publish-release-oss.sh ASSET_DIR MANIFEST TAG}
tag=${3:?usage: publish-release-oss.sh ASSET_DIR MANIFEST TAG}
: "${ACCESS_KEYID:?ACCESS_KEYID is required}"
: "${ACCESS_KEYSECRET:?ACCESS_KEYSECRET is required}"

bucket=hiroute-ai
endpoint=oss-cn-hongkong.aliyuncs.com
region=cn-hongkong
common=(--endpoint "$endpoint" --region "$region" --access-key-id "$ACCESS_KEYID" --access-key-secret "$ACCESS_KEYSECRET")

asset_list=$(mktemp)
stat_error=$(mktemp)
trap 'rm -f "$asset_list" "$stat_error"' EXIT
node apps/website/scripts/release-manifest.mjs artifacts "$manifest" --tag "$tag" > "$asset_list"

# Resolve and check the entire local publication set before the first remote write.
while IFS=$'\t' read -r filename key sha size; do
  [[ -n "$filename" && -f "$asset_dir/$filename" ]] || { echo "Missing release asset: $filename" >&2; exit 1; }
done < "$asset_list"

while IFS=$'\t' read -r filename key sha size; do
  metadata="Cache-Control:public,max-age=31536000,immutable#Content-Disposition:attachment; filename=$filename#X-Oss-Meta-Sha256:$sha"
  if facts=$(aliyun oss stat "oss://$bucket/$key" "${common[@]}" 2> "$stat_error"); then
    grep -Eiq "^X-Oss-Meta-Sha256[[:blank:]]*:[[:blank:]]*$sha[[:space:]]*$" <<<"$facts" || { echo "Existing immutable object has a different SHA256: $key" >&2; exit 1; }
    grep -Eiq "^Content-Length[[:blank:]]*:[[:blank:]]*$size[[:space:]]*$" <<<"$facts" || { echo "Existing immutable object has a different size: $key" >&2; exit 1; }
    echo "Immutable release object already exists: $key"
    continue
  fi
  if ! grep -Eiq 'NoSuchKey|StatusCode[^0-9]*404|404[^0-9]+Not Found' "$stat_error"; then
    cat "$stat_error" >&2
    echo "Could not determine whether immutable release object exists: $key" >&2
    exit 1
  fi
  aliyun oss cp "$asset_dir/$filename" "oss://$bucket/$key" --meta "$metadata" "${common[@]}"
  facts=$(aliyun oss stat "oss://$bucket/$key" "${common[@]}")
  grep -Eiq "^X-Oss-Meta-Sha256[[:blank:]]*:[[:blank:]]*$sha[[:space:]]*$" <<<"$facts" || { echo "Uploaded object lacks expected SHA256 metadata: $key" >&2; exit 1; }
  grep -Eiq "^Content-Length[[:blank:]]*:[[:blank:]]*$size[[:space:]]*$" <<<"$facts" || { echo "Uploaded object size does not match: $key" >&2; exit 1; }
done < "$asset_list"
