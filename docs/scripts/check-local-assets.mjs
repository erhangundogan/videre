import { access, readdir, readFile } from 'node:fs/promises';
import { dirname, extname, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ASSET_REFERENCE = /\b(?:href|src)=["']([^"']+)["']/g;

async function htmlFiles(root) {
	const files = [];

	async function walk(directory) {
		for (const entry of await readdir(directory, { withFileTypes: true })) {
			const path = resolve(directory, entry.name);
			if (entry.isDirectory()) await walk(path);
			else if (entry.isFile() && extname(entry.name) === '.html') files.push(path);
		}
	}

	await walk(root);
	return files;
}

export async function checkLocalAssets(outputDirectory) {
	const root = resolve(outputDirectory);
	const pages = await htmlFiles(root);
	const missing = [];
	let references = 0;

	for (const htmlPath of pages) {
		const html = await readFile(htmlPath, 'utf8');
		for (const match of html.matchAll(ASSET_REFERENCE)) {
			const reference = match[1];
			if (/^(?:[a-z]+:)?\/\//i.test(reference) || reference.startsWith('data:')) continue;

			const pathname = decodeURIComponent(reference.split(/[?#]/, 1)[0]);
			if (!['.css', '.js'].includes(extname(pathname))) continue;

			const assetPath = pathname.startsWith('/')
				? resolve(root, `.${pathname}`)
				: resolve(dirname(htmlPath), pathname);
			const fromRoot = relative(root, assetPath);
			if (fromRoot.startsWith('..') || fromRoot === '') continue;

			references += 1;
			try {
				await access(assetPath);
			} catch {
				missing.push(`${relative(root, htmlPath)} references missing asset ${reference}`);
			}
		}
	}

	if (missing.length > 0) {
		throw new Error(`Generated documentation has missing local assets:\n${missing.sort().join('\n')}`);
	}

	return { htmlFiles: pages.length, references };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
	const outputDirectory = process.argv[2] ?? fileURLToPath(new URL('../dist/', import.meta.url));
	const result = await checkLocalAssets(outputDirectory);
	console.log(`Verified ${result.references} local CSS/JS references across ${result.htmlFiles} pages.`);
}
