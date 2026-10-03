#!/bin/bash
# Release CI only. Never disable Gatekeeper or remove quarantine attributes.
set -euo pipefail
# Keep the imported P12 and signing scratch files private even on a shared host.
umask 077
missing_settings=()
for setting in APPLE_CERTIFICATE_BASE64 APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_ID APPLE_TEAM_ID APPLE_APP_PASSWORD RELEASE_REPOSITORY RELEASE_SOURCE_SHA RUNNER_TEMP; do
  if [[ -z "${!setting:-}" ]]; then
    missing_settings+=("$setting")
  fi
done
if (( ${#missing_settings[@]} )); then
  printf 'Signing is not configured. Missing release settings: %s\n' "${missing_settings[*]}" >&2
  echo 'Configure Apple credentials in the protected release environment. Private unsigned preparation does not need these credentials.' >&2
  exit 1
fi
python3 scripts/release_identity.py
mkdir unsigned-review
tar -xf unsigned-macos-review.tar -C unsigned-review
python3 scripts/package_release.py --verify unsigned-review --revision "$RELEASE_SOURCE_SHA" --repository "$RELEASE_REPOSITORY" --require-qualified
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
# Preserve the qualified unsigned package and its checksums for provenance.
release_binary="$RUNNER_TEMP/delm-signed"
cp unsigned-review/plugins/delm/bin/delm "$release_binary"
codesign --force --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY" --keychain "$release_keychain" "$release_binary"
codesign --verify --strict --verbose=2 "$release_binary"
ditto -c -k --keepParent "$release_binary" "$RUNNER_TEMP/delm-notarization.zip"
xcrun notarytool submit "$RUNNER_TEMP/delm-notarization.zip" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" --wait --output-format json > "$RUNNER_TEMP/delm-notarization.json"
python3 - "$RUNNER_TEMP/delm-notarization.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1]))
if result.get("status") != "Accepted" or not result.get("id"):
    raise SystemExit("Apple did not accept this notarization submission; inspect the retained response.")
print("Notarization accepted:", result["id"])
PY
# Apple recommends codesign's notarized requirement for non-app code. Standalone
# Mach-O tools cannot staple a ticket; this checks the online notarization record.
codesign --verify --strict --check-notarization -R=notarized "$release_binary"
cp "$release_binary" "$RUNNER_TEMP/delm-quarantine-check"
xattr -w com.apple.quarantine '0081;00000000;DeLMReleaseQualification;' "$RUNNER_TEMP/delm-quarantine-check"
"$RUNNER_TEMP/delm-quarantine-check" --version
python3 scripts/package_release.py --runtime "$release_binary" --output signed-release --repository "$RELEASE_REPOSITORY" --revision "$RELEASE_SOURCE_SHA" --unsigned-origin unsigned-review --signed-and-notarized
