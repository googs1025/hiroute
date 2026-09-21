#!/usr/bin/env bash
set -euo pipefail

destination=${1:?usage: install-aliyun-cli.sh DESTINATION_DIRECTORY}
mkdir -p "$destination"
archive="$RUNNER_TEMP/aliyun-cli-linux-amd64.tgz"
extract="$RUNNER_TEMP/aliyun-cli-extract"
mkdir -p "$extract"
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
  https://aliyuncli.alicdn.com/aliyun-cli-linux-latest-amd64.tgz --output "$archive"
tar -xzf "$archive" -C "$extract" aliyun
install -m 0755 "$extract/aliyun" "$destination/aliyun"
"$destination/aliyun" version
