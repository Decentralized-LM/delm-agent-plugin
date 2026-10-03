#!/bin/bash
# Release CI only. Never disable Gatekeeper or remove quarantine attributes.
set -euo pipefail
for setting in APPLE_CERTIFICATE_BASE64 APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_ID APPLE_TEAM_ID APPLE_APP_PASSWORD RELEASE_REPOSITORY RUNNER_TEMP; do
  if [[ -z "${!setting:-}" ]]; then
    echo "Missing release secret or environment variable: $setting" >&2
    exit 1
  fi
done
release_keychain="$RUNNER_TEMP/delm-signing.keychain-db"
release_certificate="$RUNNER_TEMP/delm-signing.p12"
release_password=$(openssl rand -hex 32)
trap 'security delete-keychain "$release_keychain" >/dev/null 2>&1 || true; rm -f "$release_certificate"' EXIT
printf '%s' "$APPLE_CERTIFICATE_BASE64" | base64 --decode > "$release_certificate"
security create-keychain -p "$release_password" "$release_keychain"
security set-keychain-settings -lut 21600 "$release_keychain"
security unlock-keychain -p "$release_password" "$release_keychain"
security import "$release_certificate" -k "$release_keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -k "$release_password" "$release_keychain" >/dev/null
mkdir unsigned-review
tar -xf unsigned-macos-review.tar -C unsigned-review
python3 scripts/package_release.py --verify unsigned-review
release_binary="$PWD/unsigned-review/plugins/delm/bin/delm"
codesign --force --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY" --keychain "$release_keychain" "$release_binary"
codesign --verify --strict --verbose=2 "$release_binary"
ditto -c -k --keepParent "$release_binary" "$RUNNER_TEMP/delm-notarization.zip"
xcrun notarytool submit "$RUNNER_TEMP/delm-notarization.zip" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" --wait
# Apple recommends codesign's notarized requirement for non-app code. Standalone
# Mach-O tools cannot staple a ticket; this checks the online notarization record.
codesign --verify --strict --check-notarization -R=notarized "$release_binary"
cp "$release_binary" "$RUNNER_TEMP/delm-quarantine-check"
xattr -w com.apple.quarantine '0081;00000000;DeLMReleaseQualification;' "$RUNNER_TEMP/delm-quarantine-check"
"$RUNNER_TEMP/delm-quarantine-check" --version
python3 scripts/package_release.py --runtime "$release_binary" --output signed-release --repository "$RELEASE_REPOSITORY" --revision "$(git rev-parse HEAD)" --signed-and-notarized
