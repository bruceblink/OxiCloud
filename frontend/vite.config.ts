import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

// Backend dev server (cargo run) — the Vite dev server proxies API/protocol
// traffic here so cookies, CSRF, and the auth-refresh flow are same-origin.
const BACKEND = process.env.OXICLOUD_BACKEND ?? 'http://localhost:8086';

const proxy = {
	'/api': { target: BACKEND, changeOrigin: true },
	'/locales': { target: BACKEND, changeOrigin: true },
	'/.well-known': { target: BACKEND, changeOrigin: true },
	'/remote.php': { target: BACKEND, changeOrigin: true },
	'/ocs': { target: BACKEND, changeOrigin: true },
	'/status.php': { target: BACKEND, changeOrigin: true },
	'/webdav': { target: BACKEND, changeOrigin: true },
	'/caldav': { target: BACKEND, changeOrigin: true },
	'/carddav': { target: BACKEND, changeOrigin: true },
	'/wopi': { target: BACKEND, changeOrigin: true },
	'/magic': { target: BACKEND, changeOrigin: true }
};

export default defineConfig({
	plugins: [sveltekit()],
	server: {
		port: 5173,
		proxy
	},
	test: {
		environment: 'jsdom',
		setupFiles: ['./vitest-setup.ts'],
		include: ['src/**/*.{test,spec}.{js,ts}'],
		globals: true
	}
});
