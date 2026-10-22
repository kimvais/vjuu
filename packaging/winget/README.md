# winget

`winget install vjuu` needs a manifest in [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs).
Microsoft reviews the first version by hand; after that, the release workflow's `winget` job opens an update pull
request for every release.

## First submission (once)

After the first GitHub release exists:

```powershell
winget install komac
komac new kimvais.vjuu --version 0.1.0 `
  --urls https://github.com/kimvais/vjuu/releases/download/v0.1.0/vjuu-v0.1.0-x86_64-pc-windows-msvc.zip `
         https://github.com/kimvais/vjuu/releases/download/v0.1.0/vjuu-v0.1.0-aarch64-pc-windows-msvc.zip `
  --moniker vjuu --package-name vjuu --publisher "Kimmo Parviainen-Jalanko" --license MIT `
  --short-description "Fast review window for AI-agent handoffs" --submit
```

Komac reads the zip, finds `vjuu.exe` as a portable command, and opens the pull request from your fork of
winget-pkgs. It asks for a GitHub token with the `public_repo` scope.

## Updates

Add that token as the repository secret `WINGET_TOKEN`. Each `v*` tag then runs `komac update` in CI.
