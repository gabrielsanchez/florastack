# Hosting FloraStack's browser version

`cargo xtask web` builds the browser version into `target/web-site/`: a static site, with no server-side code. Copy
the folder's files to any web server that sends the headers below, or build the Docker image at the repository's
root, which serves it with nginx (`docker build -t florastack-web .`, then `docker run --rm -p 8080:8080
florastack-web`). The book's [In the browser](../../book/src/user/browser.md) chapter says what the browser
version does and doesn't do.

| File | What it is |
|---|---|
| `index.html` | The page. Every URL in it is relative, so the site works from any path. |
| `florastack.js`, `florastack_bg.wasm` | The app, as JavaScript glue and WebAssembly (wasm-bindgen's) |
| `worker.js` | Starts each of the app's threads, a Web Worker running the same module |
| `icon.png` | The page's icon |
| `_headers`, `.htaccess` | The headers below, for Netlify and Cloudflare Pages, and for Apache |
| `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE` | The licenses |

## Required headers

The app stacks on several threads, which share the WebAssembly module's memory. Browsers allow shared memory only on
a page that is *cross-origin isolated*, which takes two headers on the page, and on every file it loads:

```text
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

Without them, the page shows a message saying so instead of the app. Send these as well:

```text
Cross-Origin-Resource-Policy: same-origin
X-Content-Type-Options: nosniff
Cache-Control: no-cache
```

`Cross-Origin-Resource-Policy` lets an isolated page load the site's own files (use `cross-origin` when they come from
a CDN on another origin), and `no-cache` makes browsers check the files again on each visit: their names stay the
same from one version to the next, and a browser that kept an old module could pair it with new glue.

Also:

- **MIME types:** serve `.wasm` as `application/wasm` and `.js` as `text/javascript`. Most servers already do.
- **HTTPS:** browsers give threads and the origin-private file system (where spilled tiles go) only to secure pages:
  `https://`, or `http://localhost` for testing. The page can't be opened from `file://`.
- **Compression:** gzip or Brotli for `.wasm` and `.js` takes the download from about 25 MB to about 7 MB.

### nginx

[`nginx.conf`](nginx.conf) is the Docker image's server and a complete example; in a server block of your own:

```nginx
location /florastack/ {
    add_header Cross-Origin-Opener-Policy "same-origin" always;
    add_header Cross-Origin-Embedder-Policy "require-corp" always;
    add_header Cross-Origin-Resource-Policy "same-origin" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header Cache-Control "no-cache" always;
    gzip on;
    gzip_types application/wasm text/javascript application/javascript;
}
```

An `add_header` in a `location` replaces every `add_header` of the levels above it, so put all five in one place.

### Apache

[`.htaccess`](.htaccess), in the site's folder, needs mod_headers and `AllowOverride FileInfo`; the same lines work
in the server's configuration.

### Caddy

```caddy
example.org {
    root * /srv/florastack
    file_server
    encode gzip zstd
    header {
        Cross-Origin-Opener-Policy "same-origin"
        Cross-Origin-Embedder-Policy "require-corp"
        Cross-Origin-Resource-Policy "same-origin"
        X-Content-Type-Options "nosniff"
        Cache-Control "no-cache"
    }
}
```

### Netlify and Cloudflare Pages

They read [`_headers`](_headers) from the site's folder: deploy the folder as it is.

### Other hosts

Amazon CloudFront takes the headers as a response headers policy, Firebase Hosting in `firebase.json`'s `headers`,
and Vercel in `vercel.json`'s `headers`. GitHub Pages can't send headers, so the browser version doesn't run there.

### Behind a proxy, and in a frame

A reverse proxy in front of the server must pass the `Cross-Origin-*` headers on. A page that embeds the app in an
`<iframe>` must be cross-origin isolated itself, and the frame needs `allow="cross-origin-isolated"`.

## Where the users' files go

Nowhere but their browser. Frames dropped on the page or picked are read into the page's memory, the project lives
there too, and exports are downloads; spilled tiles go to the browser's origin-private file system for the site and
are removed when the page closes (or, after a crash, when the app next starts). Nothing is sent to the server.
