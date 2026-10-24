# winget

`winget install vjuu` needs a manifest in [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs).
Microsoft reviews the first version by hand; after that, the release workflow's `winget` job opens an update pull
request for every release.

## Until the first version is merged

`komac update` only works for a package that is already in winget-pkgs, so each version is submitted whole from
the manifests in `manifest/` (the answers `komac new` would ask for). After the GitHub release exists:

```powershell
winget install komac
./packaging/winget/submit.ps1 0.3.3 -DryRun   # check the manifests
./packaging/winget/submit.ps1 0.3.3
```

The script fills in the version, date and the two zips' hashes, and runs `komac submit`, which opens the pull
request from your fork of winget-pkgs. Komac asks for a GitHub token with the `public_repo` scope (or reads
`GITHUB_TOKEN`). Close the previous version's pull request if it is still open.

## Updates

Add that token as the repository secret `WINGET_TOKEN`. Each `v*` tag then runs `komac update` in CI.
