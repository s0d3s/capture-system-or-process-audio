import { defineConfig } from 'astro/config';
import { satteri } from '@astrojs/markdown-satteri';
import starlight from '@astrojs/starlight';
import { mathProcessorOptions } from './scripts/math-processor.mjs';
import starlightHeadingBadges from 'starlight-heading-badges';
import starlightImageZoom from 'starlight-image-zoom';
import starlightKbd from 'starlight-kbd';
import starlightLinksValidator from 'starlight-links-validator';
import starlightLlmsTxt from 'starlight-llms-txt';

export default defineConfig({
  site: 'https://s0d3s.github.io',
  base: '/capture-system-or-process-audio',
  trailingSlash: 'always',
  prerenderConflictBehavior: 'error',
  compressHTML: true,
  build: {
    inlineStylesheets: 'always',
  },
  prefetch: {
    prefetchAll: true,
    defaultStrategy: 'hover',
  },
  markdown: {
    processor: satteri(mathProcessorOptions),
  },
  integrations: [
    starlight({
      title: 'CSPA Docs',
      description:
        'Cross-platform system speaker loopback, single application/process, and microphone PCM audio capture & live mixer for Python, Rust, C++17, and C11.',
      favicon: '/favicon.svg',
      head: [
        {
          tag: 'script',
          content: "if (typeof localStorage !== 'undefined' && localStorage.getItem('starlight-theme') === null) localStorage.setItem('starlight-theme', 'dark');",
        },
      ],
      logo: {
        src: './public/favicon.svg',
        alt: 'capture-system-or-process-audio waveform logo',
      },
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/s0d3s/capture-system-or-process-audio',
        },
      ],
      editLink: {
        baseUrl: 'https://github.com/s0d3s/capture-system-or-process-audio/edit/main/website/',
      },
      customCss: ['./src/styles/custom.css'],
      disable404Route: true,
      routeMiddleware: './src/routeData.ts',
      markdown: {
        headingLinks: true,
      },
      expressiveCode: {
        styleOverrides: {
          borderRadius: '0.65rem',
        },
      },
      pagefind: {
        index: {
          includeCharacters: '_:.',
        },
        ranking: {
          termSimilarity: 9,
          pageLength: 0.1,
        },
      },
      components: {
        Header: './src/components/Header.astro',
        PageTitle: './src/components/PageTitle.astro',
        Search: './src/components/Search.astro',
      },
      plugins: [
        starlightLlmsTxt({
          projectName: 'capture-system-or-process-audio',
          description:
            'Cross-platform native system speaker loopback, single application/process, and microphone PCM audio capture & live mixer for Python (3.11-3.15t), Rust (1.88+), C++17, and C11 across Windows (WASAPI), Linux (PipeWire & PulseAudio), and Apple Silicon macOS (Core Audio Process Taps & ScreenCaptureKit).',
          details: [
            '- Architecture invariant: Backends capture. Core orders and buffers. Manager plans, monitors, mixes and reopens sessions. Adapters adapt.',
            '- Zero virtual audio cables required (no VB-Cable, BlackHole, or Soundflower).',
            '- Zero silent scope broadening: unsupported OS scopes fail explicitly with CapabilityError / PlanError rather than falling back to full-system capture.',
          ].join('\n'),
          minify: {
            collapseCodeBlocks: false,
          },
          customSelectors: {
            all: ['.sl-anchor-link'],
          },
          exclude: ['404'],
          customSets: [
            {
              label: 'Getting Started and Architecture',
              paths: ['getting-started/**', 'concepts/**'],
              description:
                'Core architecture invariants, targets, discovery, CspaMixer drift sync, format conversion, OS backends, permissions, and FAQ.',
            },
            {
              label: 'Python SDK',
              paths: ['sdks/python*', 'adapters/**'],
              description:
                'Python 3.11-3.15t (-Xgil=0) SDK handbook, 8 runnable recipes, PyAudioWPatch migration, NumPy/Whisper/wave/asyncio integrations, and planned adapters.',
            },
            {
              label: 'Rust SDK',
              paths: ['sdks/rust*'],
              description:
                'Safe-Rust (#![forbid(unsafe_code)]) unified cross-platform facade handbook, 0-alloc read_into, CspaMixer telemetry, and AudioStream.',
            },
            {
              label: 'C-CPP SDK',
              paths: ['sdks/cpp*', 'sdks/c*'],
              description:
                'C++17 header-only RAII (cspa.hpp, -fno-exceptions & /EHsc), CMake Cspa::cspa_ffi integration, and C11 wire ABI (cspa_experimental.h) handbooks.',
            },
            {
              label: 'Interactive Studio Demo',
              paths: ['studio-demo/**'],
              description:
                'Complete panel-by-panel, toggle-by-toggle, modal, and shortcut manual for cspa-tui-demo.',
            },
            {
              label: 'API Reference',
              paths: ['reference/**'],
              description:
                'Complete 349-symbol API reference across Python (.pyi), Safe-Rust, C++17 (cspa.hpp), and C11 (cspa_experimental.h).',
            },
          ],
          optionalLinks: [
            {
              label: 'GitHub Repository',
              url: 'https://github.com/s0d3s/capture-system-or-process-audio',
              description: 'Source code, releases, and issue tracker',
            },
            {
              label: 'PyPI Package',
              url: 'https://pypi.org/project/capture-system-or-process-audio/',
              description: 'Prebuilt Python 3.11-3.15t wheels',
            },
            {
              label: 'crates.io Package',
              url: 'https://crates.io/crates/capture-system-or-process-audio',
              description: 'Safe-Rust unified cross-platform facade crate',
            },
          ],
        }),
        starlightLinksValidator({
          errorOnInvalidHashes: true,
          errorOnRelativeLinks: false,
          components: [
            ['LinkCard', 'href'],
            ['LinkButton', 'href'],
          ],
          exclude: ['/capture-system-or-process-audio/api/rust/**'],
        }),
        starlightKbd({
          globalPicker: true,
          types: [
            { id: 'windows', label: 'Windows', detector: 'windows', default: true },
            { id: 'mac', label: 'macOS', detector: 'apple' },
            { id: 'linux', label: 'Linux', detector: 'linux' },
          ],
        }),
        starlightHeadingBadges(),
        starlightImageZoom(),
      ],
      sidebar: [
        {
          label: 'Getting Started',
          items: [
            { label: 'Overview & Architecture', slug: 'getting-started/overview' },
            { label: 'Installation', slug: 'getting-started/installation' },
            { label: 'Choosing an API', slug: 'getting-started/choosing-an-api' },
          ],
        },
        {
          label: 'Core Concepts & Manuals',
          items: [
            { label: 'Targets, Scopes & Tracking', slug: 'concepts/targets-and-scopes' },
            { label: 'Live Discovery & Subscriptions', slug: 'concepts/discovery' },
            { label: 'Planning, CspaMixer & Hot-Swap', slug: 'concepts/planning-and-mixing' },
            { label: 'Format Conversion & Dither', slug: 'concepts/format-conversion' },
            { label: 'Real-Time Safety & Lifecycle', slug: 'concepts/realtime-and-lifecycle' },
            { label: 'Platform Backends & Matrix', slug: 'concepts/platforms' },
            { label: 'Recording Permissions', slug: 'concepts/permissions' },
            { label: 'Troubleshooting & FAQ', slug: 'concepts/troubleshooting-faq' },
          ],
        },
        {
          label: 'SDK Handbooks',
          items: [
            { label: 'SDK Overview & Quick Starts', slug: 'sdks' },
            { label: 'Python (3.11–3.15t)', slug: 'sdks/python' },
            { label: 'Rust (1.88+)', slug: 'sdks/rust' },
            { label: 'C++17 RAII & CMake', slug: 'sdks/cpp' },
            { label: 'C11 Wire ABI', slug: 'sdks/c' },
          ],
        },
        {
          label: 'Ecosystem & Adapters',
          items: [{ label: 'Integrations & Drop-In Adapters', slug: 'adapters/overview' }],
        },
        {
          label: 'Studio Demo (cspa-tui-demo)',
          items: [
            { label: 'Overview & Screen Map', slug: 'studio-demo' },
            { label: 'Panels & Toggles ([1]–[5])', slug: 'studio-demo/panels-and-toggles' },
            { label: 'Modals & Shortcuts', slug: 'studio-demo/modals-and-shortcuts' },
          ],
        },
        {
          label: 'API Reference',
          items: [
            { label: 'Multi-SDK Reference Hub', slug: 'reference' },
            {
              label: 'Python API (.pyi)',
              collapsed: true,
              items: [
                { label: 'Functions & Factories', slug: 'reference/python/functions' },
                { label: 'Targets & Discovery', slug: 'reference/python/targets-and-discovery' },
                { label: 'Streams, Blocks & Plans', slug: 'reference/python/streams-and-planning' },
                { label: 'Mixer, Converter & Budgets', slug: 'reference/python/mixing-and-conversion' },
                { label: 'Permissions & 33 Enums', slug: 'reference/python/permissions-and-enums' },
              ],
            },
            {
              label: 'Rust API & rustdoc',
              collapsed: true,
              items: [
                { label: 'Facade & Crate Portals', slug: 'reference/rust/overview' },
                { label: 'Traits, Structs & Enums', slug: 'reference/rust/types-and-traits' },
                {
                  label: 'Hosted cargo doc ↗',
                  link: '/api/rust/capture_system_or_process_audio/',
                },
              ],
            },
            {
              label: 'C++17 API (cspa.hpp)',
              collapsed: true,
              items: [
                { label: 'Streams, Targets & Discovery', slug: 'reference/cpp/classes-and-streams' },
                { label: 'Config, Plan & Permissions', slug: 'reference/cpp/config-and-reports' },
                { label: 'Result<T>, Error & Enums', slug: 'reference/cpp/errors-and-enums' },
              ],
            },
            {
              label: 'C11 ABI (cspa_experimental.h)',
              collapsed: true,
              items: [
                { label: 'ABI, Handles & Functions', slug: 'reference/c/abi-and-functions' },
                { label: 'Wire Records & Constants', slug: 'reference/c/records-and-enums' },
              ],
            },
          ],
        },
      ],
    }),
  ],
});
