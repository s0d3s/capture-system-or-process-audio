import { satteriKatex } from 'satteri-katex';

export const mathProcessorOptions = {
  features: { math: true, rawHtml: true },
  mdastPlugins: [satteriKatex()],
};
