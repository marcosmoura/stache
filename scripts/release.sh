#!/usr/bin/env bash
set -euo pipefail

###############################################################################
# Stache – Release Automation
#
# Run the entire release flow with one command from the repo root:
#
#   ./scripts/release.sh
#
# What happens:
# 1. Dependencies are installed with pnpm (locked).
# 2. The Tauri bundle is produced (release by default, override via BUNDLE_PROFILE).
# 3. The CLI binary is built with cargo.
# 4. The Rust binaries are installed with `cargo install --path`.
# 5. The resulting .app bundle is copied into /Applications (sudo only if needed).
###############################################################################

APP_NAME="Stache"
APPLICATIONS_DIR="/Applications"
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAURI_DIR="${PROJECT_ROOT}/app/native"
BUNDLE_PROFILE="${BUNDLE_PROFILE:-release}" # Accepts "release" or "debug"
BUNDLE_DIR="${PROJECT_ROOT}/target/${BUNDLE_PROFILE}/bundle/macos"
BUNDLE_NAME="${APP_NAME}.app"
BUNDLE_PATH=""
INSTALL_PATH="${APPLICATIONS_DIR}/${APP_NAME}.app"
SUDO_REFRESHED=0
SIGNING_IDENTITY="Stache App"
SKIP_TESTS=0

log() {
	echo ''
	printf "\033[1m%s \033[0m\n\n" "$1"
}

progress() {
	echo ''
	printf "\033[1;33m%s \033[0m\n\n" "$1"
}

succeed() {
	echo ''
	printf "\033[1;32m%s \033[0m\n\n" "$1"
}

error() {
	echo ''
	printf "\033[1;31m%s \033[0m\n\n" "$1"
}

fail() {
	echo ''
	printf "Error: %s\n" "$1" >&2
	exit 1
}

cleanup_on_exit() {
	local status=$?
	if ((status != 0)); then
		log "Release failed (exit code ${status})."
	fi
}
trap cleanup_on_exit EXIT

require_command() {
	if ! command -v "$1" >/dev/null 2>&1; then
		fail "'$1' is required but not available in PATH."
	fi
}

assert_macos() {
	if [[ "$(uname -s)" != "Darwin" ]]; then
		fail "This release workflow only supports macOS."
	fi
}

ensure_repo_root() {
	[[ -f "${PROJECT_ROOT}/package.json" ]] || fail "package.json not found in ${PROJECT_ROOT}."
}

ensure_applications_dir() {
	[[ -d "${APPLICATIONS_DIR}" ]] || fail "Applications dir ${APPLICATIONS_DIR} does not exist."
}

discover_bundle() {
	[[ -d "${BUNDLE_DIR}" ]] || fail "Bundle directory ${BUNDLE_DIR} not found. Run the build first."

	local expected="${BUNDLE_DIR}/${BUNDLE_NAME}"
	if [[ -d "${expected}" ]]; then
		BUNDLE_PATH="${expected}"
		return
	fi

	local fallback_bundle
	fallback_bundle="$(find "${BUNDLE_DIR}" -maxdepth 1 -mindepth 1 -type d -name '*.app' -print | sort | tail -n 1 || true)"

	[[ -n "${fallback_bundle}" ]] || fail "No .app bundles were found under ${BUNDLE_DIR}."
	BUNDLE_PATH="${fallback_bundle}"
	log "Bundle ${BUNDLE_NAME} not found; using $(basename "${BUNDLE_PATH}") instead."
}

requires_privileged_access() {
	[[ ! -w "${APPLICATIONS_DIR}" ]] && return 0
	[[ -e "${INSTALL_PATH}" && ! -w "${INSTALL_PATH}" ]] && return 0
	return 1
}

refresh_sudo() {
	if ((SUDO_REFRESHED == 0)); then
		require_command sudo
		log "Escalating privileges (sudo)..."
		sudo -v
		SUDO_REFRESHED=1
	fi
}

run_with_privilege() {
	if requires_privileged_access; then
		refresh_sudo
		sudo "$@"
	else
		"$@"
	fi
}

ensure_signing_certificate() {
	# Check if the signing certificate exists
	if security find-identity -v -p codesigning 2>/dev/null | grep -q "${SIGNING_IDENTITY}"; then
		log "Code signing certificate '${SIGNING_IDENTITY}' found."
		return 0
	fi

	log "Creating code signing certificate '${SIGNING_IDENTITY}'..."

	local temp_dir
	temp_dir=$(mktemp -d)
	local key_file="${temp_dir}/key.pem"
	local cert_file="${temp_dir}/cert.pem"
	local p12_file="${temp_dir}/cert.p12"
	local password="stache-temp-pwd"

	# Create OpenSSL config for code signing
	cat >"${temp_dir}/cert.conf" <<EOF
[req]
distinguished_name = req_distinguished_name
x509_extensions = v3_req
prompt = no

[req_distinguished_name]
CN = ${SIGNING_IDENTITY}
O = Local Development
C = US

[v3_req]
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
basicConstraints = critical, CA:FALSE
subjectKeyIdentifier = hash
EOF

	# Generate private key and self-signed certificate
	openssl req -x509 -newkey rsa:2048 -keyout "${key_file}" -out "${cert_file}" \
		-days 3650 -nodes -config "${temp_dir}/cert.conf" 2>/dev/null

	# Convert to PKCS12 format (required for Keychain import)
	# Use -legacy flag if available (OpenSSL 3.x compatibility)
	if openssl pkcs12 -help 2>&1 | grep -q -- "-legacy"; then
		openssl pkcs12 -export -out "${p12_file}" -inkey "${key_file}" -in "${cert_file}" \
			-password "pass:${password}" -legacy 2>/dev/null
	else
		openssl pkcs12 -export -out "${p12_file}" -inkey "${key_file}" -in "${cert_file}" \
			-password "pass:${password}" 2>/dev/null
	fi

	# Import into login keychain
	security import "${p12_file}" -k ~/Library/Keychains/login.keychain-db \
		-P "${password}" -T /usr/bin/codesign -T /usr/bin/security 2>/dev/null

	# Set certificate as trusted for code signing
	security add-trusted-cert -d -r trustRoot -p codeSign -k ~/Library/Keychains/login.keychain-db "${cert_file}" 2>/dev/null || true

	# Allow codesign to access the key without prompting
	security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "" ~/Library/Keychains/login.keychain-db 2>/dev/null || true

	# Cleanup temp files
	rm -rf "${temp_dir}"

	# Verify certificate was created
	if security find-identity -v -p codesigning 2>/dev/null | grep -q "${SIGNING_IDENTITY}"; then
		log "Certificate '${SIGNING_IDENTITY}' created successfully."
	else
		log "Warning: Certificate creation may have failed. Checking..."
		security find-identity -v -p codesigning 2>/dev/null || true
	fi
}

sign_app() {
	local app_path="$1"

	# Check if signing identity exists
	if ! security find-identity -v -p codesigning 2>/dev/null | grep -q "${SIGNING_IDENTITY}"; then
		log "No code signing identity '${SIGNING_IDENTITY}' found. App will use ad-hoc signature."
		log "Note: Accessibility permissions will need to be re-granted after each build."
		return 0
	fi

	log "Signing app with identity: ${SIGNING_IDENTITY}"

	# Unlock keychain to allow codesign access
	security unlock-keychain -p "" ~/Library/Keychains/login.keychain-db 2>/dev/null || true

	# Ensure all files are writable (codesign needs write access)
	chmod -R u+w "${app_path}"

	# Sign all nested executables in MacOS folder first
	for file in "${app_path}/Contents/MacOS"/*; do
		if [[ -f "${file}" && -x "${file}" ]]; then
			codesign --force --sign "${SIGNING_IDENTITY}" --timestamp=none "${file}" || {
				log "Warning: Failed to sign ${file}"
			}
		fi
	done

	# Sign dylibs if any
	find "${app_path}/Contents" -type f -name "*.dylib" 2>/dev/null | while read -r file; do
		codesign --force --sign "${SIGNING_IDENTITY}" --timestamp=none "${file}" 2>/dev/null || true
	done

	# Sign frameworks if any
	if [[ -d "${app_path}/Contents/Frameworks" ]]; then
		for fw in "${app_path}/Contents/Frameworks"/*.framework; do
			if [[ -d "${fw}" ]]; then
				codesign --force --deep --sign "${SIGNING_IDENTITY}" --timestamp=none "${fw}" 2>/dev/null || true
			fi
		done
	fi

	# Sign the main app bundle
	codesign --force --sign "${SIGNING_IDENTITY}" --timestamp=none "${app_path}"

	# Verify
	if codesign --verify --deep --strict "${app_path}" 2>/dev/null; then
		log "App successfully signed and verified."
	else
		log "Warning: App signature verification failed. Accessibility permissions may need re-granting."
	fi
}

tauri_build() {
	local args=()
	if [[ "${BUNDLE_PROFILE}" != "release" ]]; then
		args+=(--debug)
	fi
	pnpm tauri build --config "${TAURI_DIR}/tauri.conf.json" "${args[@]}"
}

main() {
	cd "${PROJECT_ROOT}"

	assert_macos
	ensure_repo_root
	ensure_applications_dir

	require_command pnpm
	require_command cargo
	require_command ditto

	progress "Installing JavaScript dependencies via pnpm"
	pnpm install --frozen-lockfile

	if ((SKIP_TESTS == 0)); then
		progress "Running tests"
		pnpm run test || fail "Tests failed. Aborting release."
	else
		log "Skipping tests (--skip-tests)"
	fi

	progress "Formatting code"
	pnpm run format

	progress "Building Tauri bundle (profile=${BUNDLE_PROFILE})"
	tauri_build

	progress "Installing ${APP_NAME} binary via Cargo"
	cargo install --path "${TAURI_DIR}" --force

	discover_bundle

	progress "Ensuring code signing certificate exists"
	ensure_signing_certificate

	progress "Signing application for consistent permissions"
	sign_app "${BUNDLE_PATH}"

	progress "Copying $(basename "${BUNDLE_PATH}") into ${APPLICATIONS_DIR}"
	run_with_privilege rm -rf "${INSTALL_PATH}"
	run_with_privilege ditto "${BUNDLE_PATH}" "${INSTALL_PATH}"

	progress "Stopping any running ${APP_NAME} instances"
	pkill -f "${APP_NAME}" 2>/dev/null || true

	succeed "Release complete!"
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
	case "$1" in
	--skip-tests)
		SKIP_TESTS=1
		shift
		;;
	*)
		fail "Unknown option: $1"
		;;
	esac
done

main
