@echo off
rem =====================================================================
rem  Build vaultis on Windows and hand back a DEMO vault you can open.
rem
rem    scripts\build.bat [--release] [--fresh] [--sample-dir DIR]
rem                      [--no-sample] [--install-rust] [--no-shortcuts]
rem                      [-- <extra cargo args>]
rem
rem  It does four things:
rem    1. Makes sure a usable RUST TOOLCHAIN is installed AND new enough. If
rem       `cargo` is missing it offers to install it with the official rustup
rem       installer - https://rustup.rs; if the toolchain is older than the
rem       minimum below it offers to run `rustup update stable` instead. Either
rem       way it asks first and defaults to NO; --install-rust answers yes up
rem       front, for an unattended run.
rem    2. `cargo build` - debug by default, --release for the optimized build.
rem    3. Makes sure a FULLY POPULATED sample vault exists - every tab filled
rem       in, with attached documents so the encrypted volume is real - by
rem       running the `seed_sample_vault` example, then prints where it is and
rem       the two passwords that open it.
rem    4. Installs the two DESKTOP SHORTCUTS - "vaultis (View)" and
rem       "vaultis (Edit)" - by running packaging\windows\make-shortcuts.ps1
rem       against the exe just built. --no-shortcuts skips it, and so does
rem       --no-sample, which means "just build". A failure there is a warning,
rem       not a build failure: the exe itself is still good.
rem
rem  The sample vault is fiction - see examples\seed_sample_vault.rs: fake
rem  people, fake institutions, visibly fake "passwords". Its two master
rem  passwords are deliberately trivial - sample1 / sample2 - because it is a
rem  throwaway demo, NOT a place to put anything real. It lives under target\
rem  so `cargo clean` takes it with it.
rem
rem  Re-running is non-destructive: an existing sample vault is left exactly as
rem  it is, since you may have clicked around and saved edits. Pass --fresh to
rem  delete and rebuild it.
rem
rem  This is the Windows twin of scripts/build.sh - same flags, same defaults,
rem  same passwords. Keep the two in step.
rem
rem  Batch notes for readers used to shells:
rem    * `rem` is a comment. Inside a parenthesised block, an unescaped ( or )
rem      even in a comment ends the block early - hence the dashes above and
rem      the comments kept OUTSIDE the blocks below.
rem    * `%~1` is the first argument with any surrounding quotes stripped;
rem      `shift` drops it so the next one becomes %1.
rem    * `if errorlevel 1` is true for ANY exit code >= 1: batch's way of
rem      asking "did that command fail?".
rem =====================================================================

rem `setlocal` keeps these variables out of the caller's shell.
rem `enabledelayedexpansion` makes !VAR! read a variable's CURRENT value inside
rem a loop or block, which %VAR% cannot do - it is substituted once, when the
rem whole block is parsed.
setlocal enabledelayedexpansion

rem --- The demo vault's two passwords. Demo-only, by design. ---------------
set "SAMPLE_PW1=sample1"
set "SAMPLE_PW2=sample2"

rem %~dp0 is this script's own folder with a trailing backslash, so the script
rem works from any working directory. %%~fI turns "<dir>\.." into a real path.
set "SCRIPT_DIR=%~dp0"
for %%I in ("%SCRIPT_DIR%..") do set "REPO_ROOT=%%~fI"
pushd "%REPO_ROOT%"
if errorlevel 1 (
    echo error: cannot enter the repository root %REPO_ROOT% 1>&2
    exit /b 1
)

set "PROFILE=debug"
set "PROFILE_ARGS="
set "FRESH=0"
set "SAMPLE_DIR="
set "EXTRA_CARGO_ARGS="
set "INSTALL_RUST=0"
set "SHORTCUTS=1"

rem Oldest toolchain that can build this workspace: the crates are edition 2024,
rem which needs Rust 1.85+, and the source uses stable let-chains, 1.88+. Checked
rem rather than assumed, because an old rustc fails with an error that reads like a
rem code problem.
set "MIN_RUST_MINOR=88"

rem --- Argument parsing ---------------------------------------------------
rem Batch has no `case`, so this is a goto loop over the arguments.
:parse_args
if "%~1"=="" goto args_done
if /i "%~1"=="--release" (
    set "PROFILE=release"
    set "PROFILE_ARGS=--release"
    shift
    goto parse_args
)
if /i "%~1"=="--install-rust" (
    set "INSTALL_RUST=1"
    shift
    goto parse_args
)
if /i "%~1"=="--fresh" (
    set "FRESH=1"
    shift
    goto parse_args
)
if /i "%~1"=="--sample-dir" (
    if "%~2"=="" goto need_dir
    set "SAMPLE_DIR=%~f2"
    shift
    shift
    goto parse_args
)
rem --no-sample builds only - useful in CI, where nobody opens a demo vault or
rem wants launchers dropped on a throwaway runner's Desktop.
if /i "%~1"=="--no-sample" (
    set "SAMPLE_DIR=-"
    set "SHORTCUTS=0"
    shift
    goto parse_args
)
if /i "%~1"=="--no-shortcuts" (
    set "SHORTCUTS=0"
    shift
    goto parse_args
)
rem Everything after a bare `--` is handed to cargo build verbatim.
if "%~1"=="--" (
    shift
    goto collect_extra
)
if /i "%~1"=="--help" goto usage
if /i "%~1"=="-h" goto usage
if "%~1"=="/?" goto usage
echo error: unknown option %1 - see --help 1>&2
popd
exit /b 2

:need_dir
echo error: --sample-dir needs a DIR 1>&2
popd
exit /b 2

rem `%1` here, not `%~1`: an argument the user quoted stays quoted, so a cargo
rem argument containing spaces survives being handed on.
:collect_extra
if "%~1"=="" goto args_done
set "EXTRA_CARGO_ARGS=!EXTRA_CARGO_ARGS! %1"
shift
goto collect_extra

:args_done

rem VAULTIS_SAMPLE_DIR lets the location be set from the environment too. The
rem --sample-dir flag wins over it, and both default to target\sample-vault.
set "DEFAULT_SAMPLE_DIR=%REPO_ROOT%\target\sample-vault"
if "%SAMPLE_DIR%"=="" set "SAMPLE_DIR=%VAULTIS_SAMPLE_DIR%"
if "%SAMPLE_DIR%"=="" set "SAMPLE_DIR=%DEFAULT_SAMPLE_DIR%"

rem --- Rust toolchain -----------------------------------------------------
rem
rem Installing a compiler toolchain is a real change to the machine - it downloads
rem and runs an installer from the network - so it is never done silently: the script
rem asks, and defaults to NO. --install-rust is the explicit yes for an unattended
rem run. `where` is the Windows equivalent of `which`; it sets a nonzero exit code
rem when the program is not on PATH.
where cargo >nul 2>nul
if not errorlevel 1 goto have_cargo
rem A rustup install this console has not picked up yet: %USERPROFILE%\.cargo\bin is
rem on PATH only for consoles started AFTER the install.
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
    goto have_cargo
)
echo Rust ^(cargo^) was not found on this machine.
echo vaultis is written in Rust, so a toolchain is needed to build it.
if "%INSTALL_RUST%"=="1" goto install_rust
set "REPLY="
set /p "REPLY=Install it now with rustup (from https://rustup.rs)? [y/N] "
if /i "%REPLY%"=="y" goto install_rust
if /i "%REPLY%"=="yes" goto install_rust
echo Not installing. Install Rust yourself and re-run this script:
echo   https://rustup.rs   ^(or: winget install Rustlang.Rustup^)
popd
exit /b 1

:install_rust
rem If rustup is already here, only the default toolchain is missing.
where rustup >nul 2>nul
if not errorlevel 1 (
    echo ==^> rustup is installed but no toolchain is active; installing stable
    rustup toolchain install stable
    rustup default stable
    goto after_install
)
echo ==^> Installing the Rust toolchain with rustup ^(https://rustup.rs^)
rem Fetch the official installer over HTTPS into TEMP, run it with rustup's defaults
rem - the -y flag - then delete it. PowerShell is used only as the downloader, since
rem batch has none; -NoProfile keeps a user profile script out of the way.
rem The installer is per-architecture, so pick the one this machine can run: an ARM64
rem Windows box handed the x64 build would install a toolchain targeting the wrong CPU.
set "RUSTUP_URL=https://win.rustup.rs/x86_64"
if /i "%PROCESSOR_ARCHITECTURE%"=="ARM64" set "RUSTUP_URL=https://win.rustup.rs/aarch64"
set "RUSTUP_INIT=%TEMP%\rustup-init-vaultis.exe"
powershell -NoProfile -ExecutionPolicy Bypass -Command "$ProgressPreference='SilentlyContinue'; [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -Uri %RUSTUP_URL% -OutFile '%RUSTUP_INIT%'"
if errorlevel 1 (
    echo. 1>&2
    echo error: could not download the rustup installer. 1>&2
    echo        Install Rust yourself: https://rustup.rs 1>&2
    popd
    exit /b 1
)
"%RUSTUP_INIT%" -y
set "INSTALL_RC=%ERRORLEVEL%"
del /q "%RUSTUP_INIT%" 2>nul
if not "%INSTALL_RC%"=="0" (
    echo. 1>&2
    echo error: the rustup installer failed. 1>&2
    popd
    exit /b 1
)
rem The installer puts cargo on PATH for FUTURE consoles; add it to this one.
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

:after_install
where cargo >nul 2>nul
if errorlevel 1 (
    echo. 1>&2
    echo error: cargo is still not on PATH after installing. 1>&2
    echo        Open a NEW command prompt and run this script again. 1>&2
    popd
    exit /b 1
)

:have_cargo
rem An ancient-but-present toolchain is the other failure mode, and its error message
rem - "let chains are unstable", "edition 2024 is unsupported" - reads like broken code
rem rather than a stale compiler. So the version is checked, and an old one is offered
rem the same deal a missing one gets: the script asks, defaults to NO, and
rem --install-rust answers yes up front for an unattended run.
call :read_rust_minor
if not defined RUST_MINOR goto rust_ok
if %RUST_MINOR% GEQ %MIN_RUST_MINOR% goto rust_ok

rem Only rustup can update the toolchain for us. A Rust installed some other way - an
rem MSI, winget, a distro package - has to be updated the way it was installed.
where rustup >nul 2>nul
if errorlevel 1 goto rust_too_old_manual
echo Rust 1.%RUST_MINOR% is too old to build vaultis - it needs 1.%MIN_RUST_MINOR% or newer.
if "%INSTALL_RUST%"=="1" goto update_rust
set "REPLY="
set /p "REPLY=Update it now with rustup, and make stable the default? [y/N] "
if /i "%REPLY%"=="y" goto update_rust
if /i "%REPLY%"=="yes" goto update_rust
echo Not updating. Update it yourself and re-run this script:
echo   rustup update stable
echo Or re-run with --install-rust to update without being asked.
popd
exit /b 1

:update_rust
echo ==^> Updating the Rust toolchain ^(rustup update stable^)
rustup update stable
if errorlevel 1 goto rust_update_failed
rem Updating stable is not enough on its own when the DEFAULT toolchain is an old
rem pinned one - `rustup default 1.85.0` - because rustc would still be that old
rem compiler afterwards. Point the default at stable too, then re-read the version
rem rather than assuming the update did the job.
rustup default stable
if errorlevel 1 goto rust_update_failed
call :read_rust_minor
if not defined RUST_MINOR goto rust_ok
if %RUST_MINOR% GEQ %MIN_RUST_MINOR% goto rust_ok
echo. 1>&2
echo error: rustc is still 1.%RUST_MINOR% after updating - needs 1.%MIN_RUST_MINOR% or newer. 1>&2
echo        Something else is choosing the compiler: a rust-toolchain.toml in this or a 1>&2
echo        parent directory, a `rustup override`, or another rustc earlier on PATH. 1>&2
popd
exit /b 1

:rust_update_failed
echo. 1>&2
echo error: rustup could not update the toolchain. 1>&2
echo        Update it yourself and re-run this script:  rustup update stable 1>&2
popd
exit /b 1

:rust_too_old_manual
echo. 1>&2
echo error: Rust 1.%RUST_MINOR% is too old to build vaultis - needs 1.%MIN_RUST_MINOR% or newer. 1>&2
echo        rustup is not installed, so this script cannot update it for you. 1>&2
echo        Update your Rust installation - see https://rustup.rs 1>&2
popd
exit /b 1

:rust_ok

echo ==^> Building vaultis ^(%PROFILE%^)
cargo build --workspace %PROFILE_ARGS%%EXTRA_CARGO_ARGS%
rem Stop on a failed build rather than reporting a vault that was never built.
if errorlevel 1 (
    echo. 1>&2
    echo Build FAILED - no sample vault was created. 1>&2
    popd
    exit /b 1
)

if "%SAMPLE_DIR%"=="-" goto no_sample

set "VAULT_FILE=%SAMPLE_DIR%\vault.pmv"

if not exist "%VAULT_FILE%" goto seed
if "%FRESH%"=="0" goto already_there
rem --fresh deletes a whole directory tree, and the only thing that made this "safe"
rem was that the target contains a vault.pmv - which is exactly what a REAL vault
rem contains. So --fresh --sample-dir C:\my-vault, or the same via VAULTIS_SAMPLE_DIR,
rem would destroy real, irreplaceable data with no prompt and no backup. Refuse unless
rem the target is the default throwaway location this script owns.
if /i not "%SAMPLE_DIR%"=="%DEFAULT_SAMPLE_DIR%" goto refuse_fresh
echo ==^> Removing the existing sample vault ^(--fresh^): %SAMPLE_DIR%
rmdir /s /q "%SAMPLE_DIR%"
if exist "%VAULT_FILE%" (
    echo. 1>&2
    echo error: could not remove %SAMPLE_DIR% 1>&2
    popd
    exit /b 1
)

rem The seeder is an example, so it is built on demand and never ships inside a
rem binary. It refuses to overwrite an existing vault.pmv, which the checks
rem above already guarantee. Key derivation uses the REAL - deliberately slow -
rem parameters, so this takes a few seconds; the demo then opens exactly as a
rem real vault does.
:seed
echo ==^> Seeding a fully populated sample vault
cargo run -p vaultis %PROFILE_ARGS% --example seed_sample_vault -- "%SAMPLE_DIR%" "%SAMPLE_PW1%" "%SAMPLE_PW2%"
if errorlevel 1 (
    echo. 1>&2
    echo Seeding the sample vault FAILED. 1>&2
    popd
    exit /b 1
)
goto shortcuts

:already_there
echo ==^> Sample vault already present - left untouched; use --fresh to rebuild it
goto shortcuts

:refuse_fresh
echo. 1>&2
echo error: refusing to --fresh a vault outside the default sample location. 1>&2
echo        target:  %SAMPLE_DIR% 1>&2
echo        default: %DEFAULT_SAMPLE_DIR% 1>&2
echo        That directory holds a real vault.pmv. If you are certain it is 1>&2
echo        disposable, delete it yourself and re-run. 1>&2
popd
exit /b 1

rem The installer is pointed at the exe THIS run built - the %PROFILE% one - rather
rem than left to auto-detect: a debug build must not silently install shortcuts for a
rem stale release exe still sitting in target\release. The shortcuts therefore store a
rem path inside the build tree, so a later `cargo clean` breaks them; packaging\README.md
rem has the copy-it-somewhere-permanent recipe.
rem
rem It is best effort - a missing script, no icons, a locked Desktop - because none of
rem that means the build failed. Warn, then print the summary anyway.
:shortcuts
set "GUI_BIN=%REPO_ROOT%\target\%PROFILE%\vaultis-gui.exe"
set "CLI_BIN=%REPO_ROOT%\target\%PROFILE%\vaultis.exe"
set "SHORTCUT_PS1=%REPO_ROOT%\packaging\windows\make-shortcuts.ps1"
set "SHORTCUT_NOTE=  Desktop shortcuts: not installed - packaging\windows\make-shortcuts.ps1"
if "%SHORTCUTS%"=="0" goto summary
if not exist "%SHORTCUT_PS1%" goto shortcuts_missing
echo ==^> Installing the desktop shortcuts
powershell -NoProfile -ExecutionPolicy Bypass -File "%SHORTCUT_PS1%" -Exe "%GUI_BIN%"
if errorlevel 1 goto shortcuts_failed
set "SHORTCUT_NOTE=  Desktop shortcuts installed: vaultis (View) and vaultis (Edit)."
goto summary

:shortcuts_missing
echo warning: cannot find %SHORTCUT_PS1% - skipping the desktop shortcuts. 1>&2
goto summary

:shortcuts_failed
echo warning: installing the desktop shortcuts failed; the build itself is fine. 1>&2
echo          Retry with: powershell -ExecutionPolicy Bypass -File "%SHORTCUT_PS1%" -Exe "%GUI_BIN%" 1>&2
goto summary

rem The summary this script exists for: the LAST thing printed, after all the
rem cargo noise, so the location and the two passwords are on screen when the
rem build ends.
:summary
echo.
echo ------------------------------------------------------------------------
echo  Sample vault ready - fully populated demo data, safe to experiment in
echo ------------------------------------------------------------------------
echo   Location:    %SAMPLE_DIR%
echo   Password 1:  %SAMPLE_PW1%
echo   Password 2:  %SAMPLE_PW2%
echo.
echo   Open it - graphical, editable:
echo     "%GUI_BIN%" "%SAMPLE_DIR%" --write
echo   Open it - terminal:
echo     "%CLI_BIN%" --tui "%SAMPLE_DIR%" --write
echo.
echo %SHORTCUT_NOTE%
echo.
echo   Everything in it is fiction - never put real secrets in this vault.
echo ------------------------------------------------------------------------
popd
exit /b 0

:no_sample
echo ==^> Skipping the sample vault ^(--no-sample^)
popd
exit /b 0

:usage
echo Build vaultis and hand back a DEMO vault you can immediately open.
echo.
echo   scripts\build.bat [--release] [--fresh] [--sample-dir DIR]
echo                     [--no-sample] [--install-rust] [--no-shortcuts]
echo                     [-- ^<extra cargo args^>]
echo.
echo   --release          Build, and seed with, the optimized build.
echo   --fresh            Delete an existing sample vault and build a new one.
echo   --sample-dir DIR   Put the sample vault somewhere other than
echo                      target\sample-vault.
echo   --no-sample        Just build; skip the sample vault AND the shortcuts.
echo   --install-rust     Install the Rust toolchain if it is missing, or update it
echo                      if it is too old, without asking.
echo   --no-shortcuts     Skip installing the two Desktop shortcuts.
echo   --                 Pass the remaining arguments to cargo build.
echo.
echo The sample vault is fiction and its two passwords are deliberately trivial
echo - %SAMPLE_PW1% / %SAMPLE_PW2% - so never put anything real in it.
echo Re-running leaves an existing sample vault untouched.
popd
exit /b 0

rem Read the MINOR version out of "rustc 1.88.0 (...)" into RUST_MINOR, leaving it
rem undefined when rustc cannot be run at all. The for/f splits on spaces to get
rem "1.88.0", then on dots to get "88". A subroutine because the version is read twice:
rem once to check it, once after an update to confirm the update actually took.
:read_rust_minor
set "RUST_MINOR="
for /f "tokens=2 delims= " %%V in ('rustc --version 2^>nul') do (
    for /f "tokens=2 delims=." %%M in ("%%V") do set "RUST_MINOR=%%M"
)
exit /b 0
