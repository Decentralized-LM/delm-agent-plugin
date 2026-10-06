#!/bin/bash
# Release CI only. Never disable Gatekeeper or remove quarantine attributes.
set -euo pipefail
# Keep the imported P12 and signing scratch files private even on a shared host.
umask 077
missing_settings=()
for setting in APPLE_CERTIFICATE_BASE64 APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY RELEASE_REPOSITORY RELEASE_SOURCE_SHA RUNNER_TEMP; do
  if [[ -z "${!setting:-}" ]]; then
    missing_settings+=("$setting")
  fi
done
if (( ${#missing_settings[@]} )); then
  printf 'Signing is not configured. Missing release settings: %s\n' "${missing_settings[*]}" >&2
  echo 'Configure Apple credentials in the protected release environment. Private unsigned preparation does not need these credentials.' >&2
  exit 1
fi
release_tools="$(cd "$(dirname "$0")" && pwd)"
python3 "$release_tools/release_tool.py" release_identity.py
mkdir unsigned-review
# Preserve reviewed payload modes under the private signing umask.
tar -xpf unsigned-macos-review.tar -C unsigned-review
python3 "$release_tools/release_tool.py" package_release.py --verify unsigned-review --revision "$RELEASE_SOURCE_SHA" --repository "$RELEASE_REPOSITORY" --require-qualified
release_keychain="$RUNNER_TEMP/delm-signing.keychain-db"
release_certificate="$RUNNER_TEMP/delm-signing.p12"
release_password=$(openssl rand -hex 32)
release_search_list=()
while IFS= read -r existing_keychain; do
  release_search_list+=("$existing_keychain")
done < <(security list-keychains -d user | python3 -c 'import shlex,sys; print("\n".join(shlex.split(sys.stdin.read())))')
cleanup_signing() {
  security list-keychains -d user -s "${release_search_list[@]}" >/dev/null 2>&1 || true
  security delete-keychain "$release_keychain" >/dev/null 2>&1 || true
  rm -f "$release_certificate"
}
trap cleanup_signing EXIT
printf '%s' "$APPLE_CERTIFICATE_BASE64" | base64 --decode > "$release_certificate"
security create-keychain -p "$release_password" "$release_keychain"
security set-keychain-settings -lut 21600 "$release_keychain"
security unlock-keychain -p "$release_password" "$release_keychain"
# codesign also resolves key material through the user's keychain search list.
security list-keychains -d user -s "$release_keychain" "${release_search_list[@]}"
# Apple intermediate certificates are public; importing the issuer does not
# override system trust or authorize an untrusted root.
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer \
  --output "$RUNNER_TEMP/DeveloperIDG2CA.cer"
security import "$RUNNER_TEMP/DeveloperIDG2CA.cer" -k "$release_keychain"
security import "$release_certificate" -k "$release_keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
echo "Configuring signing-key access"
security set-key-partition-list -S apple-tool:,apple:,codesign: -k "$release_password" "$release_keychain" >/dev/null
echo "Checking signing identity"
security find-identity -v -p codesigning "$release_keychain"
# Preserve the qualified unsigned packages and their checksums for provenance.
# package_release verifies identical runtime bytes in both host payloads and
# copies this one signed binary into each self-contained host package.
release_binary="$RUNNER_TEMP/delm-signed"
cp unsigned-review/plugins/delm/bin/delm "$release_binary"
echo "Signing the qualified runtime"
codesign --force --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY" --keychain "$release_keychain" "$release_binary"
codesign --verify --strict --verbose=2 "$release_binary"
python3 "$release_tools/release_tool.py" package_release.py --runtime "$release_binary" --output signed-release --repository "$RELEASE_REPOSITORY" --revision "$RELEASE_SOURCE_SHA" --unsigned-origin unsigned-review --signed
