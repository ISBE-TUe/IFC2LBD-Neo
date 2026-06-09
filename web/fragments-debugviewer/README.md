# Fragments Debug Viewer

Minimal local viewer for testing `.frag` files.

## Run

```bash
cd web/fragments-debugviewer
docker compose up --build
```

Open `http://localhost:3001`.

## Load a file

- Use the file picker to open a local `.frag` file directly in the browser.
- Or place files under `public/models/` and load them with a path like `/models/model.frag`.

The viewer is intentionally basic: one viewport, one status box, one reset button.
