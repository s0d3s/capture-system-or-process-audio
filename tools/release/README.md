# Release tooling

![capture-system-or-process-audio](https://raw.githubusercontent.com/s0d3s/capture-system-or-process-audio/main/assets/repo_banner.svg)

The release workflow runs these scripts from the repository root. Shared target,
crate, runtime, wheel-tag, retention, and tool-version settings are in
`config.py`. When adding a Python ABI, update the corresponding wheel build and
smoke checks in `build_target.py` as well as the configuration.

Run the offline regression tests with:

```sh
python -m unittest discover -s tools/release -p 'test_*.py'
```

The build scripts require their normal platform toolchains and workflow
environment. `publish_crates.py` publishes to crates.io and should only be run
with an authorized release token.