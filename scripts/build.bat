@echo off
rem =====================================================================
rem  Build vaultis on Windows and hand back a DEMO vault you can open.
rem
rem    scripts\build.bat [--release] [--fresh] [--sample-dir DIR]
rem                      [--no-sample] [--install-rust] [--no-shortcuts]
rem                      [-- <extra cargo args>]
rem
rem  It does four things:
rem    1. Makes sure the ONE Rust toolchain this project builds with on Windows
rem       - stable-x86_64-pc-windows-gnullvm - is installed, selected for this
rem       directory, and new enough. Never msvc and never plain gnu; the long
rem       comment by RUST_TARGET below records what each of those cost. rustup
rem       can install this one unaided, so a fresh Windows box needs no Visual
rem       Studio and no multi-gigabyte Build Tools download. If rustup is
rem       missing it offers to install it - https://rustup.rs; if the toolchain
rem       is older than the minimum below it offers to update it. Both ask
rem       first and default to NO; --install-rust answers yes up front, for an
rem       unattended run.
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
rem ONE toolchain on every Windows machine: stable-x86_64-pc-windows-gnullvm. The goal
rem is a toolchain rustup can install ON ITS OWN, so a double-click setup never stops to
rem demand a separate multi-gigabyte compiler install. Both obvious candidates fail that
rem test, each in its own way, and the scars are worth recording:
rem
rem   msvc - links with link.exe, which ships with Visual Studio. A fresh Windows box has
rem     none, and rustup installs the msvc toolchain onto it regardless, noting the
rem     missing prerequisites in one warning that scrolls past in the install chatter.
rem     First real symptom: `cargo build` dying on "linker `link.exe` not found", some
rem     hundreds of crates and several minutes in.
rem
rem   gnu - carries a linker of its own (rustup's rust-mingw component) but NOT a full
rem     binutils. The windows-* crates declare their imports with `kind = "raw-dylib"`,
rem     which rustc implements on a gnu target by running dlltool, and dlltool in turn
rem     needs an assembler that rust-mingw does not ship. Symptom, later still:
rem     "dlltool.exe: CreateProcess".
rem
rem gnullvm keeps the MinGW-w64 headers and CRT but uses LLVM's tooling instead of GNU
rem binutils, so neither hole applies, and rustup distributes it as a full host
rem toolchain. Nothing here needs a C COMPILER on Windows either way: the only crates
rem with C build scripts (wayland, x11, android) are never built for a Windows target.
rem
rem Pinned on Windows-on-ARM too, which has no equally self-contained target: the x64
rem binaries it produces run under that machine's x86 emulation. One toolchain means one
rem set of build behaviour to reason about, and one thing to reproduce when it breaks.
rem
rem Installing a compiler toolchain is a real change to the machine - it downloads and
rem runs an installer from the network - so it is never done silently: the script asks,
rem and defaults to NO. --install-rust is the explicit yes for an unattended run.
rem `where` is the Windows equivalent of `which`; it sets a nonzero exit code when the
rem program is not on PATH.
set "RUST_TARGET=x86_64-pc-windows-gnullvm"
set "RUST_TOOLCHAIN=stable-%RUST_TARGET%"

rem rustup, not cargo, is what decides this: choosing a specific toolchain is rustup's
rem job, and a Rust installed any other way - an MSI, winget, a distro package - cannot
rem do it at all.
where rustup >nul 2>nul
if not errorlevel 1 goto have_rustup
rem A rustup install this console has not picked up yet: %USERPROFILE%\.cargo\bin is
rem on PATH only for consoles started AFTER the install.
if exist "%USERPROFILE%\.cargo\bin\rustup.exe" (
    set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
    goto have_rustup
)
rem Some other Rust, installed without rustup. Its toolchain cannot be pinned, so build
rem with what is there and let the link check below be the judge of whether it works.
where cargo >nul 2>nul
if not errorlevel 1 goto no_rustup
echo Rust ^(rustup^) was not found on this machine.
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
echo ==^> Installing rustup ^(https://rustup.rs^)
rem Fetch the official installer over HTTPS into TEMP, run it, then delete it.
rem PowerShell is used only as the downloader, since batch has none; -NoProfile keeps a
rem user profile script out of the way.
rem The installer is per-architecture, so pick the one this machine can RUN. That is
rem about the rustup-init binary itself, not about the toolchain it fetches - which is
rem named explicitly below rather than left to the installer's default.
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
rem --default-toolchain none: install rustup and NOTHING else. Letting it pick its own
rem default would download a full msvc toolchain - hundreds of megabytes - that this
rem project then never uses, on every fresh machine.
"%RUSTUP_INIT%" -y --default-toolchain none
set "INSTALL_RC=%ERRORLEVEL%"
del /q "%RUSTUP_INIT%" 2>nul
if not "%INSTALL_RC%"=="0" (
    echo. 1>&2
    echo error: the rustup installer failed. 1>&2
    popd
    exit /b 1
)
rem The installer puts rustup on PATH for FUTURE consoles; add it to this one.
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

:have_rustup
rem Named explicitly, every run, rather than trusted to be whatever a previous run or a
rem previous rustup left behind. rustup-init also defers to an existing
rem %USERPROFILE%\.rustup\settings.toml over its own flags - saying so only in a warning
rem lost in the install chatter - so on any machine that has ever had rustup, asking the
rem installer for a toolchain is not the same as getting it.
echo ==^> Using %RUST_TOOLCHAIN%
rustup toolchain install %RUST_TOOLCHAIN%
if errorlevel 1 goto toolchain_failed
rem `override set` pins it for THIS DIRECTORY. Directory-scoped rather than `rustup
rem default`, because a machine-wide default is someone's choice for all their other
rem work and a build script has no business rewriting it.
rustup override set %RUST_TOOLCHAIN%
if errorlevel 1 goto toolchain_failed
goto after_install

:toolchain_failed
echo. 1>&2
echo error: could not select the %RUST_TOOLCHAIN% toolchain. 1>&2
echo        Try it by hand and re-run this script: 1>&2
echo            rustup toolchain install %RUST_TOOLCHAIN% 1>&2
echo            rustup override set %RUST_TOOLCHAIN% 1>&2
popd
exit /b 1

:no_rustup
echo warning: rustup is not installed, so the %RUST_TOOLCHAIN% toolchain cannot be 1>&2
echo          selected; building with whatever Rust is on PATH instead. 1>&2

:after_install
where cargo >nul 2>nul
if errorlevel 1 (
    echo. 1>&2
    echo error: cargo is still not on PATH after installing. 1>&2
    echo        Open a NEW command prompt and run this script again. 1>&2
    popd
    exit /b 1
)

rem --- Put the toolchain's own binutils on PATH ----------------------------
rem Some Windows toolchains keep their binutils inside the toolchain rather than on
rem PATH, at <sysroot>\lib\rustlib\<target>\bin\self-contained, while rustc looks them
rem up by bare name - which is how a gnu build reaches the windows-* crates and dies on
rem   error: error calling dlltool 'dlltool.exe': program not found
rem about a hundred crates in. Prepended when the directory is really there, so this is
rem simply a no-op for a toolchain that needs nothing from it.
rem
rem The sysroot is ASKED OF rustc rather than assembled from %USERPROFILE%\.rustup\...,
rem which is merely the default location and is wrong wherever RUSTUP_HOME was moved.
for /f "delims=" %%S in ('rustc --print sysroot 2^>nul') do set "RUST_SYSROOT=%%S"
if defined RUST_SYSROOT (
    if exist "!RUST_SYSROOT!\lib\rustlib\%RUST_TARGET%\bin\self-contained\" (
        set "PATH=!RUST_SYSROOT!\lib\rustlib\%RUST_TARGET%\bin\self-contained;!PATH!"
    )
)

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
set /p "REPLY=Update it now with rustup? [y/N] "
if /i "%REPLY%"=="y" goto update_rust
if /i "%REPLY%"=="yes" goto update_rust
echo Not updating. Update it yourself and re-run this script:
echo   rustup update %RUST_TOOLCHAIN%
echo Or re-run with --install-rust to update without being asked.
popd
exit /b 1

:update_rust
rem Named, not `rustup update stable`: this repository is pinned to %RUST_TOOLCHAIN% by
rem the override set above, so updating the bare `stable` channel would update a
rem DIFFERENT toolchain on any machine whose host triple is not the one we pin to -
rem leaving rustc here exactly as old as it was.
echo ==^> Updating the Rust toolchain ^(rustup update %RUST_TOOLCHAIN%^)
rustup update %RUST_TOOLCHAIN%
if errorlevel 1 goto rust_update_failed
rem Re-read the version rather than assuming the update did the job.
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

rem --- Can this toolchain actually LINK? ----------------------------------
rem A safety net rather than the main defence: the toolchain selected above brings its
rem own linker, so this normally passes. It is here for the routes that get around that
rem - a Rust installed without rustup (the warning above), or a rust-toolchain.toml in
rem some parent directory quietly selecting an msvc toolchain instead.
rem
rem "cargo is installed and new enough" does not mean it can produce a binary, and the
rem difference is expensive to discover late: failing here costs a second, while failing
rem inside `cargo build` costs ~300 downloaded crates and several minutes first.
rem
rem Tested by actually linking a two-token program, NOT by looking for link.exe on PATH:
rem a working Visual Studio install does not put link.exe on PATH - cargo finds it
rem through the registry - so `where link.exe` would reject perfectly good machines.
set "LINKCHECK_DIR=%TEMP%\vaultis-linkcheck"
rmdir /s /q "%LINKCHECK_DIR%" 2>nul
mkdir "%LINKCHECK_DIR%" 2>nul
rem Redirection first so no trailing space lands in the file; ^( ^) escapes the parens.
> "%LINKCHECK_DIR%\linkcheck.rs" echo fn main^(^) {}
rustc -o "%LINKCHECK_DIR%\linkcheck.exe" "%LINKCHECK_DIR%\linkcheck.rs" >"%LINKCHECK_DIR%\rustc.log" 2>&1
if not errorlevel 1 goto linker_ok

echo. 1>&2
echo error: this Rust toolchain cannot link a program on this machine. 1>&2
type "%LINKCHECK_DIR%\rustc.log" 1>&2
echo. 1>&2
echo        vaultis builds with %RUST_TOOLCHAIN%, which brings its own linker and 1>&2
echo        needs no Visual Studio. Select it here and re-run this script: 1>&2
echo            rustup toolchain install %RUST_TOOLCHAIN% 1>&2
echo            rustup override set %RUST_TOOLCHAIN% 1>&2
rmdir /s /q "%LINKCHECK_DIR%" 2>nul
popd
exit /b 1

:linker_ok
rmdir /s /q "%LINKCHECK_DIR%" 2>nul

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
