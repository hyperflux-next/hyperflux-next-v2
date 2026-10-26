# Releasing

1. On `dev`, run `scripts/set-version X.Y.Z`. It changes the files that
   name the version and adds entries to the Debian and RPM changelogs.
   Write the `## X.Y.Z` section of `CHANGELOG.md`. It becomes the
   release notes, which means it has to cover every distribution. End it
   with the fingerprints of the project signing key: the install guides
   send people there to check the key they downloaded. Run
   `python3 -m pytest tests` and open a pull request.
2. Once CI is green, merge `dev` into `main` with a merge commit, never a
   squash. That keeps `dev`'s commits in `main`'s history, and the next
   merge brings nothing but what's new.
3. Tag the merge commit on `main` with a signed tag and push it:
   `git tag -s vX.Y.Z -m "HyperFlux Next V2 X.Y.Z"`, `git push origin vX.Y.Z`.
4. The tag starts `release.yml`. It builds and install-tests all the
   packages from the tag and assembles the unsigned repository, with the
   previous release carried over. Then it opens a draft release with the
   changelog section. Download the site:
   `gh run download RUN_ID --name site-unsigned --dir site`.
5. Sign it, then check it with nothing but the public key, as
   [signing](signing.md) describes:
   `scripts/repo-sign.sh site`, then `scripts/repo-verify.sh site`.
6. Try it on hardware before anyone else gets it. Serve the site locally
   (`python3 -m http.server -d site 8000`) and point a test system's
   repository entry at `http://localhost:8000/arch` (or the other
   distribution's directory). Update, reboot, and run
   `hyperfluxctl doctor`, `status` and the apps.
7. `scripts/repo-publish.sh site` shows the gh-pages commit it would make.
   `scripts/repo-publish.sh site --push` pushes it and uploads the signed
   packages and `SHA256SUMS` to the draft release.
8. Check the draft's assets and notes, then publish it on GitHub.

When the distributions' `openrazer-daemon` moves to a new OpenRazer
release, the weekly workflow fails its install tests. Update the OpenRazer
pin and the patch, and the versions the driver package provides, then
release a new version.

Dependabot opens its updates against `dev` on Mondays: the GitHub Actions,
the crates and the build images, one pull request each. An update within
the same major version merges itself once every check `dev` requires has
passed (`.github/workflows/dependabot-merge.yml`). A major one waits for
review. They reach `main` with the next release. The automatic merge
needs `dev` to require those checks, which a free plan allows for a
public repository but not a private one.
