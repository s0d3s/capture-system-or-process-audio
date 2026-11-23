import { defineRouteMiddleware } from '@astrojs/starlight/route-data';

function hasMathMarkup(rawBody?: string, renderedHtml?: string): boolean {
  if (renderedHtml && renderedHtml.includes('class="katex')) {
    return true;
  }
  if (!rawBody || !rawBody.includes('$')) {
    return false;
  }
  const withoutFences = rawBody.replace(/^\s*(`{3,}|~{3,})[\s\S]*?^\s*\1/gm, '');
  const withoutInlineCode = withoutFences.replace(/`[^`\n]+`/g, '');
  return /\$\$[\s\S]+?\$\$|\$[^\s$\n][^$\n]*\$/.test(withoutInlineCode);
}

export const onRequest = defineRouteMiddleware((context) => {
  const { starlightRoute } = context.locals;
  const { entry, id } = starlightRoute;

  const siteOrigin = (context.site?.origin ?? 'https://s0d3s.github.io').replace(/\/$/, '');
  const basePath = import.meta.env.BASE_URL.replace(/\/$/, '');
  const rawPath = context.url.pathname;
  const normalizedPath = rawPath.endsWith('/') ? rawPath : `${rawPath}/`;
  const canonicalUrl = `${siteOrigin}${normalizedPath}`;
  const ogImageUrl = `${siteOrigin}${basePath}/og-card-1280x640.png`;

  const pageTitle = entry?.data?.title ?? 'capture-system-or-process-audio';
  const pageDescription =
    entry?.data?.description ??
    'Cross-platform system speaker loopback, single application/process, and microphone PCM audio capture & live mixer for Python, Rust, C++17, and C11.';

  // Remove default tags we replace deterministically so zero duplicate meta/canonical tags appear
  starlightRoute.head = starlightRoute.head.filter(({ tag, attrs }) => {
    if (tag === 'link' && attrs?.rel === 'canonical') return false;
    if (tag === 'meta' && attrs?.property === 'og:image') return false;
    if (tag === 'meta' && attrs?.property === 'og:url') return false;
    if (tag === 'meta' && attrs?.name === 'twitter:card') return false;
    if (tag === 'meta' && attrs?.name === 'twitter:image') return false;
    if (tag === 'meta' && attrs?.name === 'description') return false;
    return true;
  });

  const isHome = id === '' || id === 'index';
  const graphNodes: Record<string, unknown>[] = [];

  if (isHome) {
    graphNodes.push(
      {
        '@type': 'SoftwareSourceCode',
        name: 'capture-system-or-process-audio',
        description: pageDescription,
        url: canonicalUrl,
        codeRepository: 'https://github.com/s0d3s/capture-system-or-process-audio',
        license: [
          'https://opensource.org/licenses/MIT',
          'https://www.apache.org/licenses/LICENSE-2.0',
        ],
        programmingLanguage: ['Python', 'Rust', 'C++', 'C'],
        operatingSystem: ['Windows 10', 'Windows 11', 'Linux', 'macOS'],
        author: {
          '@type': 'Person',
          name: 's0d3s',
          url: 'https://github.com/s0d3s',
        },
      },
      {
        '@type': 'WebSite',
        name: 'capture-system-or-process-audio Documentation',
        url: `${siteOrigin}${basePath}/`,
      }
    );
  } else {
    const segments = normalizedPath
      .slice(basePath.length)
      .split('/')
      .filter(Boolean);
    const breadcrumbItems = [
      {
        '@type': 'ListItem',
        position: 1,
        name: 'Docs',
        item: `${siteOrigin}${basePath}/`,
      },
      ...segments.map((seg, idx) => ({
        '@type': 'ListItem',
        position: idx + 2,
        name: idx === segments.length - 1 ? pageTitle : seg.replace(/-/g, ' '),
        item: `${siteOrigin}${basePath}/${segments.slice(0, idx + 1).join('/')}/`,
      })),
    ];

    graphNodes.push(
      {
        '@type': 'TechArticle',
        headline: pageTitle,
        description: pageDescription,
        url: canonicalUrl,
        author: {
          '@type': 'Person',
          name: 's0d3s',
          url: 'https://github.com/s0d3s',
        },
      },
      {
        '@type': 'BreadcrumbList',
        itemListElement: breadcrumbItems,
      }
    );
  }

  const faqItems = (entry?.data as { faq?: { question: string; answer: string }[] } | undefined)?.faq;
  if (faqItems && faqItems.length > 0) {
    graphNodes.push({
      '@type': 'FAQPage',
      mainEntity: faqItems.map((q) => ({
        '@type': 'Question',
        name: q.question,
        acceptedAnswer: {
          '@type': 'Answer',
          text: q.answer,
        },
      })),
    });
  }

  const jsonLdPayload = JSON.stringify({
    '@context': 'https://schema.org',
    '@graph': graphNodes,
  });

  if (hasMathMarkup(entry?.body, entry?.rendered?.html)) {
    starlightRoute.head.push({
      tag: 'link',
      attrs: { rel: 'stylesheet', href: `${basePath}/katex/katex.min.css` },
    });
  }

  starlightRoute.head.push(
    { tag: 'meta', attrs: { name: 'description', content: pageDescription } },
    { tag: 'link', attrs: { rel: 'canonical', href: canonicalUrl } },
    { tag: 'meta', attrs: { property: 'og:url', content: canonicalUrl } },
    { tag: 'meta', attrs: { property: 'og:image', content: ogImageUrl } },
    { tag: 'meta', attrs: { property: 'og:image:width', content: '1280' } },
    { tag: 'meta', attrs: { property: 'og:image:height', content: '640' } },
    { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
    { tag: 'meta', attrs: { name: 'twitter:image', content: ogImageUrl } },
    { tag: 'script', attrs: { type: 'application/ld+json' }, content: jsonLdPayload }
  );
});
