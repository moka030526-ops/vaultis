@echo off
rem =====================================================================
rem  Build vaultis on Windows and hand back a DEMO vault you can open.
rem
rem    scripts\build.bat [--release] [--fresh] [--sample-dir DIR]
rem                      [--no-sample] [-- <extra cargo args>]
rem
rem  It does two things:
rem    1. `cargo build` - debug by default, --release for the optimized build.
rem    2. Makes sure a FULLY POPULATED sample vault exists - every tab filled
rem       in, with attached documents so the encrypted volume is real - by
rem       running the `seed_sample_vault` example, then prints where it is and
rem       the two passwords that open it.
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
rem --no-sample builds only - useful in CI, where nobody opens a demo vault.
if /i "%~1"=="--no-sample" (
    set "SAMPLE_DIR=-"
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
if "%SAMPLE_DIR%"=="" set "SAMPLE_DIR=%VAULTIS_SAMPLE_DIR%"
if "%SAMPLE_DIR%"=="" set "SAMPLE_DIR=%REPO_ROOT%\target\sample-vault"

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
goto summary

:already_there
echo ==^> Sample vault already present - left untouched; use --fresh to rebuild it
goto summary

rem The summary this script exists for: the LAST thing printed, after all the
rem cargo noise, so the location and the two passwords are on screen when the
rem build ends.
:summary
set "GUI_BIN=%REPO_ROOT%\target\%PROFILE%\vaultis-gui.exe"
set "CLI_BIN=%REPO_ROOT%\target\%PROFILE%\vaultis.exe"
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
echo                     [--no-sample] [-- ^<extra cargo args^>]
echo.
echo   --release          Build, and seed with, the optimized build.
echo   --fresh            Delete an existing sample vault and build a new one.
echo   --sample-dir DIR   Put the sample vault somewhere other than
echo                      target\sample-vault.
echo   --no-sample        Just build; skip the sample vault entirely.
echo   --                 Pass the remaining arguments to cargo build.
echo.
echo The sample vault is fiction and its two passwords are deliberately trivial
echo - %SAMPLE_PW1% / %SAMPLE_PW2% - so never put anything real in it.
echo Re-running leaves an existing sample vault untouched.
popd
exit /b 0
