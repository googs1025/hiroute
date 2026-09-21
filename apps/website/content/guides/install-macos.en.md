# Install and open HiRoute on macOS

HiRoute Desktop currently supports macOS 15.0 or later. Near-term releases are self-signed: they do not carry an Apple Developer ID signature or notarization, so macOS asks you to explicitly approve the first launch.

## Download the right installer

Choose a DMG for your Mac on the [download page](/en/download/):

- Choose `arm64` for Apple silicon (M1, M2, M3, M4, and later).
- Choose `x86_64` for an Intel Mac.

The download must come from `hiroute.ai/releases/<version>/`; its filename includes the abbreviated source revision. Verify it against the full SHA256 digest on the download page:

```sh
shasum -a 256 ~/Downloads/HiRoute-*.dmg
```

Do not open a file whose digest does not match. Delete it and download it again.

## Install

1. Open the DMG.
2. Drag HiRoute into Applications.
3. Open HiRoute from Applications instead of continuing to run it from the DMG.

## Open a self-signed build for the first time

Try opening HiRoute normally first. If macOS blocks it:

1. Open System Settings → Privacy & Security.
2. Find the blocked HiRoute launch and choose Open Anyway.
3. Confirm that you want to open it.

Apple maintains the current steps in [Safely open apps on your Mac](https://support.apple.com/en-gb/102445). HiRoute does not ask you to disable Gatekeeper, weaken system security, or disable SIP.

If macOS says the file is damaged, verify the download domain and SHA256 and download it again. Do not treat an integrity failure as an ordinary first-launch warning.

## Install the Terminal entry (optional)

After opening Desktop, go to Settings → CLI → Terminal entry and select Install. It installs the entry paired with the current app at:

```text
$HOME/.local/bin/hiroute
```

If Settings says that directory is not on PATH, add this line to `~/.zprofile`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Open a new Terminal window, then run:

```sh
hiroute --help
```

The Terminal entry connects to HiRoute's local service. Keep Desktop installed and the local service available. See [HiRoute CLI](/en/docs/cli/) for the released commands.
