// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

const REPO = 'https://github.com/erhangundogan/videre';
const SITE = 'https://docs.videre.sh';

// https://astro.build/config
export default defineConfig({
	// Used for canonical URLs and the Pagefind search index. Must match the
	// deployed origin, or sitemap and social links point at the wrong host.
	site: SITE,

	// Cautions moved from "Start here" to Reference on 2026-08-20. The old URL
	// was published and may be linked from outside, so it redirects rather than
	// 404ing.
	redirects: {
		'/start/cautions/': '/reference/cautions/',
	},
	integrations: [
		starlight({
			title: 'videre',
			description:
				'Find any photo by describing it, by who is in it, or where it was taken. Duplicates, semantic search, faces and places over a folder you already own, entirely offline.',
			// Horizontal lockup: the mark and wordmark side by side fit the header
			// bar far better than the stacked square version, which forced the
			// header taller to stay legible. The square logos are still used by
			// scripts/make-og-image.mjs, whose card is laid out around them, and
			// they must stay static: sharp rasterises SVG through librsvg, which
			// does not run SMIL, so an animated source would be captured in its
			// t=0 state with all three rays stacked on the vertical. The homepage
			// hero uses the animated pair instead; the OG card keeps these.
			logo: {
				light: './src/assets/logo-horizontal-light.svg',
				dark: './src/assets/logo-horizontal-dark.svg',
				replacesTitle: true,
			},
			favicon: '/favicon.svg',
			// Starlight emits twitter:card=summary_large_image but no image, which
			// renders a large empty box on every platform that honours it. One
			// static card, regenerated with `yarn og`, is far better than none.
			head: [
				// iOS ignores SVG favicons entirely, and an added-to-home-screen
				// tile is composited straight onto the user's wallpaper, so unlike
				// the favicon it cannot follow the system theme: the background is
				// baked in. A dark tile holds up against light and dark wallpapers
				// alike, where a white one tends to disappear against a light one.
				{
					tag: 'link',
					attrs: { rel: 'apple-touch-icon', sizes: '180x180', href: '/apple-touch-icon.png' },
				},
				{ tag: 'meta', attrs: { property: 'og:image', content: `${SITE}/og.png` } },
				{ tag: 'meta', attrs: { property: 'og:image:width', content: '1200' } },
				{ tag: 'meta', attrs: { property: 'og:image:height', content: '630' } },
				{
					tag: 'meta',
					attrs: {
						property: 'og:image:alt',
						content: 'videre: local-first photo and video library CLI',
					},
				},
				{ tag: 'meta', attrs: { name: 'twitter:image', content: `${SITE}/og.png` } },
			],
			social: [{ icon: 'github', label: 'GitHub', href: REPO }],
			editLink: { baseUrl: `${REPO}/edit/main/docs/` },
			lastUpdated: true,
			// Search is Pagefind, built at compile time and served as static
			// assets. No account, no API key, and nothing leaves the reader's
			// browser, which matches what the tool itself promises.
			sidebar: [
				{
					label: 'Start here',
					items: [
						{ label: 'What videre is', slug: 'index' },
						{ label: 'Install', slug: 'start/install' },
						{ label: 'Quickstart', slug: 'start/quickstart' },
						{ label: 'Workflows', slug: 'start/workflows' },
					],
				},
				{
					label: 'Commands',
					items: [
						{ label: 'Overview', slug: 'commands' },
						{ label: 'import', slug: 'commands/import' },
						{ label: 'scan', slug: 'commands/scan' },
						{ label: 'dedupe', slug: 'commands/dedupe' },
						{ label: 'gallery', slug: 'commands/gallery' },
						{ label: 'search', slug: 'commands/search' },
						{ label: 'embed', slug: 'commands/embed' },
						{ label: 'faces', slug: 'commands/faces' },
						{ label: 'classify', slug: 'commands/classify' },
						{ label: 'mark', slug: 'commands/mark' },
						{ label: 'locations', slug: 'commands/locations' },
						{ label: 'fix-dates', slug: 'commands/fix-dates' },
						{ label: 'prune', slug: 'commands/prune' },
						{ label: 'watch', slug: 'commands/watch' },
						{ label: 'stats', slug: 'commands/stats' },
						{ label: 'config', slug: 'commands/config' },
						{ label: 'mcp', slug: 'commands/mcp' },
					],
				},
				{
					label: 'Guides',
					items: [
						{ label: 'Leaving Google Photos', slug: 'guides/leaving-google-photos' },
						{ label: 'Browsing and labeling', slug: 'guides/browsing' },
						{ label: 'Compositional searches', slug: 'guides/compositional-search' },
						{ label: 'Scoping a run', slug: 'guides/scoping-a-run' },
						{ label: 'Long-running jobs', slug: 'guides/long-running-jobs' },
						{ label: 'Keeping libraries separate', slug: 'guides/multiple-libraries' },
						{ label: 'Using several search models', slug: 'guides/multiple-models' },
						{ label: 'Caches and disk use', slug: 'guides/caches' },
						{ label: 'Backing up', slug: 'guides/backup' },
						{ label: 'JSONL output', slug: 'guides/jsonl' },
						{ label: 'Tuning', slug: 'guides/tuning' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'Troubleshooting', slug: 'reference/troubleshooting' },
						{ label: 'Cautions', slug: 'reference/cautions' },
						{ label: 'Where your data lives', slug: 'reference/paths' },
						{ label: 'The database', slug: 'reference/database' },
						{ label: 'Platform support', slug: 'reference/platforms' },
						{ label: 'Supported files', slug: 'reference/file-types' },
						{ label: 'Search models', slug: 'reference/models' },
					],
				},
			],
		}),
	],
});
