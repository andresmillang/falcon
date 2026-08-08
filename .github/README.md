# CI workflow

The GitHub Actions workflow for this repo lives at `ci/github-actions-ci.yml`.
It was placed here (not under `.github/workflows/`) because it was pushed with a
token lacking the `workflow` scope. To enable CI, move it back:

    mkdir -p .github/workflows
    git mv ci/github-actions-ci.yml .github/workflows/ci.yml
    git commit -m "enable CI" && git push   # needs a token with 'workflow' scope
