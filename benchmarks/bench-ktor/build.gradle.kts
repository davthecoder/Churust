// Builds one fat jar, so `run.sh` starts the server the same way it starts the
// other four: one process, one command, no build tool in the measured path.

plugins {
    kotlin("jvm") version "2.4.10"
    application
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("io.ktor:ktor-server-core-jvm:3.5.2")
    implementation("io.ktor:ktor-server-netty-jvm:3.5.2")
    // Ktor logs through SLF4J and prints a loud warning on every start without
    // a binding. The no-op binding keeps the measurement free of logging work
    // — the other four apps write nothing per request either.
    implementation("org.slf4j:slf4j-nop:2.0.18")
}

kotlin {
    jvmToolchain(21)
}

application {
    mainClass.set("MainKt")
}

// A single self-contained jar. The alternative — a classpath assembled from the
// Gradle cache at start time — makes the run depend on a directory layout that
// is not part of this repository.
tasks.register<Jar>("fatJar") {
    archiveFileName.set("bench-ktor.jar")
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    manifest { attributes["Main-Class"] = "MainKt" }
    from(sourceSets.main.get().output)
    dependsOn(configurations.runtimeClasspath)
    from({
        configurations.runtimeClasspath.get()
            .filter { it.name.endsWith("jar") }
            .map { zipTree(it) }
    }) {
        // Every signed dependency ships these; keeping them makes the JVM
        // reject the merged jar as tampered with.
        exclude("META-INF/*.SF", "META-INF/*.DSA", "META-INF/*.RSA")
    }
}
