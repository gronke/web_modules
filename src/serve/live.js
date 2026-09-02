/*! web_modules live reload client. Served by the dev server at <prefix>/live.js.
 *
 * Works as a classic `<script src>` (the dev server injects one) and via `import()`
 * (a host that gates the endpoint behind its own login imports it once signed in).
 * The endpoint prefix comes from the script's own URL, else from a
 * `<meta name="web-modules-live" content="<prefix>">`, else the crate default.
 *
 * A `css` change hot-swaps the matching <link rel="stylesheet"> (every stylesheet when
 * the change names none) and dispatches `web-modules:css-reloaded` on the document;
 * everything else reloads the page in `full` mode and only logs in `css` mode. */
(() => {
	const DEFAULT_PREFIX = '/_web_modules/live';
	const current = typeof document !== 'undefined' ? document.currentScript : null;
	const meta = document.querySelector('meta[name="web-modules-live"]');
	let prefix = DEFAULT_PREFIX;
	if (current && current.src) {
		prefix = new URL(current.src, location.href).pathname.replace(/\/live\.js$/, '');
	} else if (meta && meta.content) {
		prefix = meta.content.replace(/\/$/, '');
	}
	const eventsUrl = `${prefix}/events`;
	const RETRY_MIN_MS = 1000;
	const RETRY_MAX_MS = 5000;

	let session = null;
	let mode = 'full';
	let retryDelay = RETRY_MIN_MS;
	let source = null;

	const log = (...args) => console.info('[web-modules]', ...args);

	function samePath(href, url) {
		try {
			return new URL(href, location.href).pathname === new URL(url, location.href).pathname;
		} catch {
			return false;
		}
	}

	/* Swap the stylesheet(s) for `url`: all of them when the change carries no URL or
	 * none matches (a partial edit the server could not attribute). The fresh link
	 * loads next to the old one, which goes only once the new one is in, so nothing
	 * flashes unstyled. */
	function swapStylesheets(url) {
		const links = [...document.querySelectorAll('link[rel~="stylesheet"][href]')];
		const matching = url ? links.filter(link => samePath(link.href, url)) : [];
		const targets = matching.length ? matching : links;
		for (const link of targets) {
			const fresh = link.cloneNode(false);
			const next = new URL(link.href, location.href);
			next.searchParams.set('live', String(Date.now()));
			fresh.href = next.href;
			fresh.addEventListener('load', () => {
				link.remove();
				document.dispatchEvent(new CustomEvent('web-modules:css-reloaded', { detail: { url: next.pathname } }));
			}, { once: true });
			fresh.addEventListener('error', () => fresh.remove(), { once: true });
			link.parentNode.insertBefore(fresh, link.nextSibling);
		}
	}

	function onChange(change) {
		if (change.kind === 'css') {
			swapStylesheets(change.url);
		} else if (mode === 'full') {
			location.reload();
		} else {
			log(`${change.url || 'a source file'} changed; reload to apply`);
		}
	}

	function onHello(hello) {
		if (session !== null && hello.session !== session) {
			// The server restarted (a rebuild, most likely): the page may be stale.
			if (hello.reload === 'full') {
				location.reload();
				return;
			}
			log('dev server restarted; reload to pick up changes');
		}
		session = hello.session;
		mode = hello.reload;
	}

	function connect() {
		source = new EventSource(eventsUrl);
		source.addEventListener('hello', e => onHello(JSON.parse(e.data)));
		source.addEventListener('change', e => onChange(JSON.parse(e.data)));
		source.onopen = () => {
			retryDelay = RETRY_MIN_MS;
		};
		source.onerror = () => {
			// A network hiccup reconnects by itself (CONNECTING). An HTTP error, a host
			// that answers 403 before login, closes the source for good, so reconnect
			// with a small backoff until the stream is allowed through.
			if (source.readyState === EventSource.CLOSED) {
				source.close();
				setTimeout(connect, retryDelay);
				retryDelay = Math.min(retryDelay * 2, RETRY_MAX_MS);
			}
		};
	}

	connect();
})();
