# In the browser

FloraStack also runs in a web browser: the same desktop app, compiled to WebAssembly, on a page. Nothing is
installed and nothing leaves your computer. The frames you give the page stay in it, the page stacks them on your
own processor or graphics card, and what you export comes back as downloads.

The browser version is for trying FloraStack, for a quick stack on a machine where you can't install it, and for
sites that want to offer it. For large stacks, RAW files by the hundred, projects you keep, batches and camera
control, use the [desktop app](install.md).

## What you need

- **A current browser** that runs WebAssembly on several threads. It is tested in Chrome and Edge; Firefox and
  Safari have what it needs, but haven't been tried yet.
- **A site that serves it properly.** The page needs headers that make it *cross-origin isolated*, which shared
  memory between threads requires. When they are missing, the page says so instead of starting. You can run your
  own, below.
- **For stacking on the GPU, WebGPU:** Chrome and Edge have it, as do Safari 26 and Firefox on Windows. Without it,
  FloraStack stacks on the CPU's threads and draws its window with WebGL; it stacks on the CPU too when the GPU fails
  its self-test.

## Using it

The page opens the app on the Stack tab, as the desktop app opens.

1. **Give it frames.** Drop image or RAW files on the page, or choose **File > Add frames…**, which opens the
   browser's file picker. A browser can't open folders: pick the files themselves (Ctrl+A in the picker takes a
   folder's files). **File > Import stacks from a folder…** opens the picker too, and splits the files picked into
   their stacks.
2. **Align and stack** as in [Your first stack](first-stack.md): Ctrl+L aligns, Ctrl+Enter stacks.
3. **Retouch** in the Retouch tab if the stack needs it ([Retouching](retouching.md)).
4. **Export.** In the Export tab, **Export…** writes the output with the settings above it, and the browser
   downloads the files: look in your downloads folder.

The status line says where the stacks run: `GPU: WebGPU` or `CPU`. The GPU is tested once per page, when the app
starts: until the test ends, the status line says `CPU`, and a stack started meanwhile waits for it.

## What's different

- **Nothing is kept when the page closes.** The project lives in the page's memory: **File > Save** keeps it there,
  for undo and retouching, but closing or reloading the page loses it. Export what you want to keep. Projects can't be
  opened from or saved to your folders, and there are no recent projects.
- **Memory is limited.** A page can hold at most 4 GB, the limit of WebAssembly's 32-bit memory, and the files given
  to it may take at most half of that. Files that don't fit are refused with a message that says how much they take,
  as is a stack that wouldn't fit to stack. A 20-frame stack of 24-megapixel JPEGs fits; 100 RAW files don't. Stacks
  spill tiles beyond a 512 MB budget to the browser's private file system, which the site gets for itself and which is
  emptied when the page closes.
- **No batches, watch folders or capture.** The Batch and Capture tabs, which need folders and devices, aren't there.
- **The viewer** shows images through egui rather than the desktop's own GPU viewer, so very large images are shown
  reduced.
- **Slower.** WebAssembly is slower than the desktop app's native code, and on WebGPU each frame is copied to the
  thread that owns the GPU, which costs more than it saves on small stacks: 20 frames of 480 x 360 stack in 0.5 s on
  the CPU's threads and in 1 to 4 s on WebGPU. To stack on the CPU, run **Set compute device** from the command palette
  (Ctrl+K) with `{"device": "cpu"}`.

## Running your own

The browser version is a static site with no server-side code. Build it from the source with
`cargo xtask web` ([Building and packaging the browser version](../dev/web.md)), or build the Docker image at the
repository's root, which serves it with nginx:

```sh
docker build -t florastack-web .
docker run --rm -p 8080:8080 florastack-web
```

then open <http://localhost:8080/>. Any web server works if it sends the cross-origin isolation headers on every file:

```text
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

[The hosting notes](https://github.com/gabrielsanchez/florastack/blob/main/packaging/web/README.md) give the full list
of headers, with examples for nginx, Apache, Caddy, Netlify and Cloudflare Pages, and what to watch behind a proxy
or in a frame. Serve it over HTTPS: browsers give threads to secure pages only (and to `localhost`).

## Page options

Options go after the page's address, as in `https://example.org/florastack/?threads=4`:

| Option | Effect |
|---|---|
| `threads=N` | How many threads stack (by default, as many as the browser reports cores) |
| `log=debug` | A detailed log in the browser's console (`info`, `debug` or `trace`; `warn` by default) |

## Scripts

The page has `window.florastack.dispatch(name, params)`, which runs any [engine command](../reference/commands.md)
and gives a promise of its result; a job's promise waits for its end. A script on the page, such as a browser test,
can drive the app this way: `await florastack.dispatch('align.run', {})`. Files a script writes under `/downloads/`
(an export's `folder`, say) are downloaded once no job runs.
