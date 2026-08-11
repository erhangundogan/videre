// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

const REPO = 'https://github.com/erhangundogan/videre';

// https://astro.build/config
export default defineConfig({
	// Used for canonical URLs and the Pagefind search index. Must match the
	// deployed origin, or sitemap and social links point at the wrong host.
	site: 'https://docs.videre.sh',
	integrations: [
		starlight({
			title: 'videre',
			description:
				'A local-first CLI for making sense of a folder full of photos and videos: duplicates, semantic search, faces, and places.',
			logo: {
				light: './src/assets/logo-light.svg',
				dark: './src/assets/logo-dark.svg',
				replacesTitle: true,
			},
			favicon: '/favicon.svg',
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
						{ label: 'Cautions', slug: 'start/cautions' },
					],
				},
				{
					label: 'Commands',
					items: [
						{ label: 'Overview', slug: 'commands' },
						{ label: 'scan', slug: 'commands/scan' },
						{ label: 'dedupe', slug: 'commands/dedupe' },
						{ label: 'report', slug: 'commands/report' },
						{ label: 'search', slug: 'commands/search' },
						{ label: 'embed', slug: 'commands/embed' },
						{ label: 'faces', slug: 'commands/faces' },
						{ label: 'classify', slug: 'commands/classify' },
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
						{ label: 'Browsing and labeling', slug: 'guides/browsing' },
						{ label: 'Long-running jobs', slug: 'guides/long-running-jobs' },
						{ label: 'Keeping libraries separate', slug: 'guides/multiple-libraries' },
						{ label: 'Using several search models', slug: 'guides/multiple-models' },
						{ label: 'Caches and disk use', slug: 'guides/caches' },
						{ label: 'Backing up', slug: 'guides/backup' },
						{ label: 'JSONL output', slug: 'guides/jsonl' },
					],
				},
				{
					label: 'Reference',
					items: [
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
