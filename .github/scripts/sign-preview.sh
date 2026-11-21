#!/usr/bin/env bash
# Signs one Instabox Preview build. It checks that a zipped app from Preview build is
# Instabox Preview for one architecture, signs it with the Developer ID certificate,
# notarizes and staples it, and wraps it in a signed .dmg. It never runs the app.
#
# Usage: sign-preview.sh <preview.zip> <arm64|x86_64> <out.dmg>
# Environment: APPLE_CERTIFICATE (base64 .p12), APPLE_CERTIFICATE_PASSWORD,
# APPLE_SIGNING_IDENTITY, APPLE_API_ISSUER, APPLE_API_KEY, APPLE_API_PRIVATE_KEY.
set -euo pipefail

ID="sh.instabox.bridge.preview"
NAME="Instabox Preview"

zip=$1
arch=$2
out=$3

fail() {
  echo "sign-preview: $*" >&2
  exit 1
}

work=$(mktemp -d)
kc="$work/signing.keychain-db"
keychains=$(security list-keychains -d user | tr -d '"')
cleanup() {
  # shellcheck disable=SC2086
  security list-keychains -d user -s $keychains
  security delete-keychain "$kc" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

# The build comes from pull request code, so check it is the app this signs and nothing else.
mkdir "$work/in"
ditto -x -k "$zip" "$work/in"
entries=("$work/in"/*)
[ ${#entries[@]} -eq 1 ] && [ "${entries[0]}" = "$work/in/$NAME.app" ] || fail "the zip must hold only $NAME.app"
app="$work/in/$NAME.app"
[ -z "$(find "$app" -type l)" ] || fail "the app must not hold symbolic links"
plist="$app/Contents/Info.plist"
got_id=$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$plist")
[ "$got_id" = "$ID" ] || fail "identifier is $got_id, not $ID"
got_name=$(/usr/libexec/PlistBuddy -c "Print :CFBundleName" "$plist")
[ "$got_name" = "$NAME" ] || fail "name is $got_name, not $NAME"
exe=$(/usr/libexec/PlistBuddy -c "Print :CFBundleExecutable" "$plist")
got_arch=$(lipo -archs "$app/Contents/MacOS/$exe")
[ "$got_arch" = "$arch" ] || fail "architecture is $got_arch, not $arch"

# A throwaway keychain holds the certificate for this run only.
pass=$(uuidgen)
security create-keychain -p "$pass" "$kc"
security set-keychain-settings -lut 3600 "$kc"
security unlock-keychain -p "$pass" "$kc"
printf '%s' "$APPLE_CERTIFICATE" | base64 --decode > "$work/cert.p12"
security import "$work/cert.p12" -k "$kc" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign >/dev/null
rm "$work/cert.p12"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$pass" "$kc" >/dev/null
# shellcheck disable=SC2086
security list-keychains -d user -s "$kc" $keychains

# The same signature as a release: hardened runtime and a secure timestamp.
codesign --force --options runtime --timestamp --keychain "$kc" --sign "$APPLE_SIGNING_IDENTITY" "$app"
codesign --verify --strict "$app"

umask 077
printf '%s' "$APPLE_API_PRIVATE_KEY" > "$work/notary.p8"
ditto -c -k --keepParent "$app" "$work/notarize.zip"
xcrun notarytool submit "$work/notarize.zip" --key "$work/notary.p8" --key-id "$APPLE_API_KEY" \
  --issuer "$APPLE_API_ISSUER" --wait --output-format json > "$work/notary.json"
status=$(plutil -extract status raw -o - "$work/notary.json")
if [ "$status" != "Accepted" ]; then
  id=$(plutil -extract id raw -o - "$work/notary.json" 2>/dev/null || true)
  if [ -n "$id" ]; then
    xcrun notarytool log "$id" --key "$work/notary.p8" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" >&2 || true
  fi
  fail "notarization finished as $status"
fi
xcrun stapler staple "$app"

mkdir "$work/dmg"
ditto "$app" "$work/dmg/$NAME.app"
ln -s /Applications "$work/dmg/Applications"
mkdir -p "$(dirname "$out")"
hdiutil create -volname "$NAME" -srcfolder "$work/dmg" -ov -format UDZO "$out" >/dev/null
codesign --force --timestamp --keychain "$kc" --sign "$APPLE_SIGNING_IDENTITY" "$out"

# The same check as a release: a Developer ID signature and a stapled ticket.
mnt="$work/mnt"
mkdir "$mnt"
hdiutil attach "$out" -nobrowse -readonly -mountpoint "$mnt" >/dev/null
codesign --verify --strict "$mnt/$NAME.app"
info=$(codesign -dvv "$mnt/$NAME.app" 2>&1)
grep -q "^Authority=Developer ID Application" <<<"$info" || fail "the app is not signed with a Developer ID"
xcrun stapler validate "$mnt/$NAME.app"
hdiutil detach "$mnt" >/dev/null
echo "Signed $out"
