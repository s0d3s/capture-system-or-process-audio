#!/usr/bin/env node
/**
 * Cross-platform `DOCS_RS=1` `cargo doc` builder and `/api/rust/` static asset stager.
 *
 * Documents all 5 publishable Rust workspace crates with `-D warnings` into a
 * shared target directory so cross-crate re-exports and search indexes merge
 * cleanly, then stages `doc/` into `website/public/api/rust/` ahead of `astro build`.
 */

import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const websiteRoot = path.resolve(__dirname, '..');
const repoRoot = path.resolve(websiteRoot, '..');
const targetDir = path.join(repoRoot, 'target', 'docs-site-rustdoc');
const publicRustApiDir = path.join(websiteRoot, 'public', 'api', 'rust');

const PUBLISHED_CRATE_DIRS = [
  'capture-core',
  'capture-windows',
  'capture-linux',
  'capture-macos',
  'capture',
];

function resolveEnv() {
  const env = {
    ...process.env,
    DOCS_RS: '1',
    RUSTDOCFLAGS: '-D warnings',
  };

  if (process.platform === 'win32' && !env.CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER) {
    try {
      const sysroot = execFileSync('rustc', ['--print', 'sysroot'], {
        encoding: 'utf8',
      }).trim();
      const rustLld = path.join(
        sysroot,
        'lib',
        'rustlib',
        'x86_64-pc-windows-msvc',
        'bin',
        'rust-lld.exe',
      );
      if (existsSync(rustLld)) {
        env.CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = rustLld;
      }
    } catch {
      // Fall back to default Cargo linker if sysroot inspection fails.
    }
  }

  return env;
}

function runCargoDoc(crateFolder, env) {
  const manifestPath = path.join(repoRoot, 'crates', crateFolder, 'Cargo.toml');
  const baseArgs = [
    'doc',
    '--locked',
    '--no-deps',
    '--manifest-path',
    manifestPath,
    '--target-dir',
    targetDir,
  ];

  if (!process.env.CI) {
    try {
      execFileSync('cargo', [...baseArgs, '--offline'], {
        cwd: repoRoot,
        env,
        stdio: 'inherit',
      });
      return;
    } catch {
      // Retry without --offline when uncached dependencies need initial fetch.
    }
  }

  execFileSync('cargo', baseArgs, {
    cwd: repoRoot,
    env,
    stdio: 'inherit',
  });
}

const env = resolveEnv();
for (const crateFolder of PUBLISHED_CRATE_DIRS) {
  console.log(`[build-rustdoc] Documenting crates/${crateFolder}...`);
  runCargoDoc(crateFolder, env);
}

const generatedDocDir = path.join(targetDir, 'doc');
const expectedFacadeIndex = path.join(
  generatedDocDir,
  'capture_system_or_process_audio',
  'index.html',
);
if (!existsSync(expectedFacadeIndex)) {
  throw new Error(`Expected rustdoc output missing: ${expectedFacadeIndex}`);
}

rmSync(publicRustApiDir, { recursive: true, force: true });
mkdirSync(path.dirname(publicRustApiDir), { recursive: true });
cpSync(generatedDocDir, publicRustApiDir, { recursive: true });

const rootRedirectHtml = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>CSPA Rust API Documentation</title>
    <meta http-equiv="refresh" content="0; url=capture_system_or_process_audio/index.html" />
    <link rel="canonical" href="https://s0d3s.github.io/capture-system-or-process-audio/api/rust/capture_system_or_process_audio/index.html" />
  </head>
  <body>
    <p>Redirecting to <a href="capture_system_or_process_audio/index.html">capture_system_or_process_audio rustdoc</a>...</p>
  </body>
</html>
`;
writeFileSync(path.join(publicRustApiDir, 'index.html'), rootRedirectHtml, 'utf8');
console.log(`[build-rustdoc] Mounted 5-crate rustdoc tree at ${publicRustApiDir}`);
