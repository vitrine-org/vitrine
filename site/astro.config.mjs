// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// https://astro.build/config
export default defineConfig({
	site: 'https://vitrine.frascella.dev',
	integrations: [
		starlight({
			title: 'Vitrine',
			description: 'Lightweight, interactive desktop widgets for Windows, drawn beneath your icons.',
			logo: { src: './src/assets/vitrine.svg' },
			favicon: '/favicon.svg',
			defaultLocale: 'root',
			locales: {
				root: { label: 'English', lang: 'en' },
				it: { label: 'Italiano', lang: 'it' },
			},
			social: [
				{ icon: 'github', label: 'GitHub', href: 'https://github.com/frascella-dev/vitrine' },
			],
			editLink: {
				baseUrl: 'https://github.com/frascella-dev/vitrine/edit/main/site/',
			},
			customCss: ['./src/styles/custom.css'],
			sidebar: [
				{
					label: 'Getting started',
					translations: { it: 'Per iniziare' },
					items: [
						{ slug: 'docs/introduction' },
						{ slug: 'docs/roadmap' },
					],
				},
				{
					label: 'Concepts',
					translations: { it: 'Concetti' },
					items: [{ slug: 'docs/architecture' }],
				},
			],
		}),
	],
});
