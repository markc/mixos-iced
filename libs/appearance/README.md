# Appearance

MixOS glue for the generic widget toolkit. Ced, DOpus and Term share the
rendered design-pair conversion. Ced and DOpus register the verified asset
set through toolkit's caller-supplied font and icon types, including extra
italic and fallback faces. The toolkit has no dependency on this adapter.

The token conversion and regression tests retain the frozen cosmix
`e0297242305f3a3c3de09f1ca01e8faa771768da` implementation, adapted to palette
and metrics fields. Missing pairs or invalid radius units fail explicitly.
