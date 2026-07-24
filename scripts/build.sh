#!/usr/bin/env bash
#
# Build vaultis and hand back a DEMO vault you can immediately open.
#
#   scripts/build.sh [--release] [--fresh] [--sample-dir DIR] [-- <extra cargo args>]
#
# It does two things:
#   1. `cargo build` (debug by default, `--release` for the optimized build).
#   2. Makes sure a FULLY POPULATED sample vault exists — every tab filled in, with
#      attached documents so the encrypted volume is real — by running the
#      `seed_sample_vault` example, then prints where it is and the two passwords
#      that open it.
#
# The sample vault is fiction (see examples/seed_sample_vault.rs): fake people, fake
# institutions, visibly fake "passwords". Its two master passwords are deliberately
# trivial — `sample1` / `sample2` — because it is a throwaway demo, NOT a place to put
# anything real. It lives under `target/` so `cargo clean` takes it with it.
#
# Re-running is non-destructive: an existing sample vault is left exactly as it is (you
# may have clicked around and saved edits). Pass `--fresh` to delete and rebuild it.
set -euo pipefail

# --- The demo vault's two passwords. Demo-only, by design (see above). ---------
SAMPLE_PW1="sample1"
SAMPLE_PW2="sample2"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

profile="debug"
cargo_profile_args=()
fresh=0
sample_dir=""
extra_cargo_args=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            profile="release"
            cargo_profile_args=(--release)
            shift
            ;;
        --fresh)
            fresh=1
            shift
            ;;
        --sample-dir)
            [[ $# -ge 2 ]] || { echo "error: --sample-dir needs a DIR" >&2; exit 2; }
            sample_dir="$2"
            shift 2
            ;;
        --no-sample)
            # Build only — useful in CI, where nobody clicks around a demo vault.
            sample_dir="-"
            shift
            ;;
        --)
            shift
            extra_cargo_args=("$@")
            break
            ;;
        -h|--help)
            # Print this file's leading comment block (everything after the shebang up to
            # the first non-comment line) as the help text, so there is one description.
            awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *)
            echo "error: unknown option '$1' (see --help)" >&2
            exit 2
            ;;
    esac
done

# `${VAULTIS_SAMPLE_DIR:-}` lets the location be set from the environment too; the
# `--sample-dir` flag wins over it, and both default to target/sample-vault.
if [[ -z "$sample_dir" ]]; then
    sample_dir="${VAULTIS_SAMPLE_DIR:-$repo_root/target/sample-vault}"
fi

echo "==> Building vaultis ($profile)"
cargo build --workspace "${cargo_profile_args[@]}" "${extra_cargo_args[@]}"

if [[ "$sample_dir" == "-" ]]; then
    echo "==> Skipping the sample vault (--no-sample)"
    exit 0
fi

vault_file="$sample_dir/vault.pmv"

if [[ -e "$vault_file" && $fresh -eq 1 ]]; then
    echo "==> Removing the existing sample vault (--fresh): $sample_dir"
    rm -rf -- "$sample_dir"
fi

if [[ -e "$vault_file" ]]; then
    echo "==> Sample vault already present (left untouched; use --fresh to rebuild it)"
else
    echo "==> Seeding a fully populated sample vault"
    # The seeder is an example, so it is built on demand and never ships in a binary.
    # It refuses to overwrite an existing vault.pmv, which the check above already
    # guarantees. Key derivation uses the REAL (deliberately slow) KDF parameters, so
    # this step takes a few seconds — the demo then opens exactly as a real vault does.
    cargo run -p vaultis "${cargo_profile_args[@]}" --example seed_sample_vault -- \
        "$sample_dir" "$SAMPLE_PW1" "$SAMPLE_PW2"
fi

gui_bin="$repo_root/target/$profile/vaultis-gui"
cli_bin="$repo_root/target/$profile/vaultis"

# The summary the whole script exists for: it is the LAST thing printed, after all the
# cargo noise, so the two passwords and the location are on screen when the build ends.
cat <<EOF

────────────────────────────────────────────────────────────────────────
 Sample vault ready — fully populated demo data, safe to experiment in
────────────────────────────────────────────────────────────────────────
  Location:    $sample_dir
  Password 1:  $SAMPLE_PW1
  Password 2:  $SAMPLE_PW2

  Open it (graphical, editable):
    "$gui_bin" "$sample_dir" --write
  Open it (terminal):
    "$cli_bin" --tui "$sample_dir" --write

  Everything in it is fiction — never put real secrets in this vault.
────────────────────────────────────────────────────────────────────────
EOF
