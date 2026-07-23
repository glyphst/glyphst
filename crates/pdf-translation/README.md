# Glyphst PDF translation engine

`glyphst-pdf-translation` is the application-independent PDF translation engine. It owns:

- native PDF inspection and rich text/style extraction;
- PaddleOCR normalization and native-first OCR fusion;
- semantic translation units and protected inline style markers;
- source-derived layout, overflow policy, Typst rendering, and PDF composition.

It intentionally does not depend on GPUI, Glyphst settings, or Glyphst's provider service. The
`glyphst-pdf-translation-app` crate is the thin application adapter for those concerns.

## Local sample gate

Run the deterministic CJK layout fixture against the sample repository:

```sh
cargo run -p glyphst-pdf-translation --example sample_visual_gate -- \
  ../translation-pdf-samples \
  target/pdf-translation-visual
```

The fixture is deliberately not a linguistic translator. It preserves protected content and inline
style boundaries while producing repeatable CJK-shaped text, so geometry, typography, source
cleanup, and overflow can be tested without network access or API keys. The run is strict: any
block that cannot fit at its normal minimum size fails with a `GLYPHST_OVERFLOW` diagnostic.

The application uses adaptive fitting and, if a translated block still cannot fit, preserves that
one source block instead of failing the entire PDF. The deterministic gate remains strict so layout
regressions cannot hide behind that fallback.

## Design reference

The implementation is informed by the layout-preserving approach in the local
`/home/meowking/proj/probe/retain-pdf` reference project. Glyphst keeps its own document model,
protected-region policy, translation validation, and application adapter so its viewer workflow
and sample-specific regressions remain first-class requirements.
