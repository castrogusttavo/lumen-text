---
title: Settings reference
description: Every setting, its default and what it does, generated from the editor's settings model.
section: Customize
order: 1
visual: settings
generated: settings
---

Open the settings with `Ctrl+,`. Changes apply immediately and are saved in `~/.config/lumen-text/config.json`; the file is plain JSON and the keys below are its names.

```json
{
  "theme": "vitesse-black",
  "format_on_save": true,
  "updates": "notify"
}
```

The tables below are generated from the settings model and the defaults in `src/config.rs`.

## Language servers

The *Language servers* section of the settings window lists every server of the registry with its state and a switch; the `language_servers` object in `config.json` is the full interface (add, replace or switch off servers, set arguments, environment, `initialization_options` and `settings`). It has no row of its own in the tables above because its keys are server names. See [Language servers](/docs/language-servers) for the schema.
