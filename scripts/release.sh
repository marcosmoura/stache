#!/usr/bin/env bash
set -euo pipefail

###############################################################################
# Barba – Release Automation
#
# Run the entire release flow with one command from the repo root:
#
#   ./scripts/release.sh
#
# What happens:
# 1. Dependencies are installed with pnpm (locked).
# 2. The Tauri bundle is produced (release by default, override via BUNDLE_PROFILE).
# 3. The Rust binary is installed with `cargo install --path ./src-tauri`.
# 4. The resulting .app bundle is copied into /Applications (sudo only if needed).
###############################################################################

APP_NAME="Barba"
APPLICATIONS_DIR="/Applications"
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAURI_DIR="${PROJECT_ROOT}/src-tauri"
BUNDLE_PROFILE="${BUNDLE_PROFILE:-release}" # Accepts "release" or "debug"
BUNDLE_DIR="${TAURI_DIR}/target/${BUNDLE_PROFILE}/bundle/macos"
BUNDLE_NAME="${APP_NAME}.app"
BUNDLE_PATH=""
INSTALL_PATH="${APPLICATIONS_DIR}/${APP_NAME}.app"
SUDO_REFRESHED=0

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

tauri_build() {
	local args=()
	if [[ "${BUNDLE_PROFILE}" != "release" ]]; then
		args+=(--debug)
	fi
	pnpm tauri build "${args[@]}"
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

	progress "Running tests"
	pnpm run test || fail "Tests failed. Aborting release."

	progress "Building Tauri bundle (profile=${BUNDLE_PROFILE})"
	tauri_build

	progress "Installing ${APP_NAME} binary via Cargo"
	cargo install --path "${TAURI_DIR}"

	discover_bundle

	progress "Copying $(basename "${BUNDLE_PATH}") into ${APPLICATIONS_DIR}"
	run_with_privilege rm -rf "${INSTALL_PATH}"
	run_with_privilege ditto "${BUNDLE_PATH}" "${INSTALL_PATH}"

	succeed "Release complete! ${APP_NAME} is available in ${APPLICATIONS_DIR}."
}

main "$@"
