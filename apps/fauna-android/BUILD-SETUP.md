# Android Build Setup

## Requirements

- **JDK 17** — `sudo apt install -y openjdk-17-jdk-headless`
- **Android SDK** — platform-tools, build-tools 35.0.0, platform android-35

## ARM64 (aarch64) Host Setup

Android SDK build tools are x86-64 only. On ARM64 hosts (e.g., Apple Silicon via the virtualization host/UTM), you need QEMU user-mode emulation and x86-64 cross-libraries:

```bash
# QEMU user-mode: translates x86-64 binaries transparently
sudo apt install -y qemu-user-static binfmt-support

# x86-64 dynamic linker and libraries
sudo apt install -y libc6-amd64-cross libgcc-s1-amd64-cross libstdc++6-amd64-cross

# Symlinks so x86-64 binaries find their libraries
sudo mkdir -p /lib64
sudo ln -sf /usr/x86_64-linux-gnu/lib/ld-linux-x86-64.so.2 /lib64/ld-linux-x86-64.so.2
sudo mkdir -p /lib/x86_64-linux-gnu
sudo ln -sf /usr/x86_64-linux-gnu/lib/*.so* /lib/x86_64-linux-gnu/
```

## SDK Installation (ZFS layout at /work)

```bash
sudo zfs create work/android-sdk
sudo zfs create work/gradle-cache
sudo chown $USER:$USER /work/android-sdk /work/gradle-cache

mkdir -p /work/android-sdk/cmdline-tools
curl -o /tmp/cmdline-tools.zip https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip
unzip /tmp/cmdline-tools.zip -d /work/android-sdk/cmdline-tools
mv /work/android-sdk/cmdline-tools/cmdline-tools /work/android-sdk/cmdline-tools/latest

export ANDROID_HOME=/work/android-sdk
export PATH=$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH

yes | sdkmanager --licenses
sdkmanager "platform-tools" "platforms;android-35" "build-tools;35.0.0"
```

## Environment Variables

Add to `~/.bashrc`:

```bash
export ANDROID_HOME=/work/android-sdk
export GRADLE_USER_HOME=/work/gradle-cache
export PATH=$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH
```

## Building

```bash
./gradlew assembleDebug
```

## FFI bindings

The app depends on `com.fauna.ffi.*` (UniFFI-generated bindings from `libs/fauna-ffi`).
There is no checked-in stub: the `just android-ffi*` recipes (`android-ffi`,
`android-ffi-test`, `android-ffi-store-safe`, `android-ffi-foss`)
cross-compile fauna-ffi for Android (aarch64-linux-android / x86_64-linux-android) and
stage the generated Kotlin under `app/src/<buildType>/java/com/fauna/ffi/` (git-ignored).
The `just android-debug` / `android-release` / `android-store-safe` / `android-foss`
recipes depend on the matching FFI recipe, so run those rather than `./gradlew` alone.
