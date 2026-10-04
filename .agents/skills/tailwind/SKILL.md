---
name: tailwind
description: Regenerate Tailwind CSS and fix build.rs watch triggers
---

Regenerate Tailwind CSS for `crates/lw-app` from the repository root:

```bash
npx @tailwindcss/cli -i crates/lw-app/input.css -o crates/lw-app/tailwind.generated.css --minify
```

Run this command when the task requires regenerating Tailwind CSS, then confirm that `crates/lw-app/tailwind.generated.css` was updated.
