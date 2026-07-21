# Continuous Integration

- Always impose rules on maximum number of lines in a file or maximum cyclomatic complexity from the very beginning of a project so tech debt accumulates less quickly and to reduce the need for major overhauls right before release.
- Always implement CI and CD from the very beginning of the software project, so they aren't tacked on as an afterthought. Always use Github Actions for CI and CD.
- Always keep pre-commit and pre-push hooks consistent with CI and CD so there is never a case where we're surprised that something passed a pre-commit or pre-push hook but it failed CI or CD in a way that should have been caught by the hook.
