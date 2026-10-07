pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        gradlePluginPortal()
        mavenCentral()
        // Compose Multiplatform dev artifacts.
        maven("https://maven.pkg.jetbrains.space/public/p/compose/dev")
    }
}

plugins {
    // Auto-provisions a JDK toolchain if the requested one isn't found.
    id("org.gradle.toolchains.foojay-resolver-convention") version "0.9.0"
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
        maven("https://maven.pkg.jetbrains.space/public/p/compose/dev")
    }
}

rootProject.name = "vaultis-mobile"
include(":composeApp")

// CodeQL's java-kotlin analysis traces the compiler; a compile task restored from the
// build cache never invokes kotlinc, and the scan then fails with "could not
// automatically build any of it". Under the CodeQL tracer, always compile for real.
if (System.getenv().keys.any { it.startsWith("CODEQL_") }) {
    buildCache { local { isEnabled = false } }
}
