import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';

import { checkLocalAssets } from './check-local-assets.mjs';

async function fixture(html, assets = []) {
	const root = await mkdtemp(join(tmpdir(), 'videre-doc-assets-'));
	await writeFile(join(root, 'index.html'), html);

	for (const asset of assets) {
		const path = join(root, asset);
		await mkdir(dirname(path), { recursive: true });
		await writeFile(path, 'fixture');
	}

	return root;
}

test('accepts generated HTML whose local CSS and JavaScript assets exist', async () => {
	const root = await fixture(
		'<link rel="stylesheet" href="/_astro/ec.present.css"><script src="/_astro/app.present.js"></script>',
		['_astro/ec.present.css', '_astro/app.present.js'],
	);

	await assert.doesNotReject(checkLocalAssets(root));
});

test('rejects generated HTML that references a missing local asset', async () => {
	const root = await fixture('<link rel="stylesheet" href="/_astro/ec.missing.css">');

	await assert.rejects(checkLocalAssets(root), /index\.html references missing asset \/_astro\/ec\.missing\.css/);
});
