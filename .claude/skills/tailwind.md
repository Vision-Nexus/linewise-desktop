---
name: tailwind
description: Regenerate Tailwind CSS and fix build.rs watch triggers
---

Regenerate the Tailwind CSS for `crates/lw-app`:

```bash
cd /home/kakaz/linewise-desktop/crates/lw-app && npx @tailwindcss/cli -i input.css -o tailwind.generated.css --minify
```

Run this command now. After it completes, confirm to the user that `tailwind.generated.css` has been updated.
