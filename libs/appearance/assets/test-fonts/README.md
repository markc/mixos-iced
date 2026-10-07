<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Pinned variable font fixtures

These unmodified files are copies of the approved immutable asset entries in
`share/assets/core.conf.mix`. Their original OFL licences accompany them.
Tests use real weight ranges, including the desktop's numeric weight 300,
rather than substituting static weight 400 and silently changing the policy.

| File | Upstream revision | SHA-256 |
|---|---|---|
| NotoSans.ttf | google/fonts `9710da1eacb3be272583c3224dcb70f9da6eadbb` | `bfb7bb691513f12e734dc346c03a03f784912432d7e3fa8e56efcf906fe86b3d` |
| NotoSans-OFL.txt | same | `cee9892f9f0cc8fe882c9e9537ee6a89621d86ee7ceaf70b02e2b2b1c25c061a` |
| JetBrainsMono.ttf | google/fonts `cd5227bd1f61dff3bbd6c814ceaf7ffd95e947d9` | `662a196d58f1183bf2d77428b6d5283fe3f45161ab021bea4036bc98e5cac016` |
| JetBrainsMono-OFL.txt | same | `30f0c136e3c88e422d0791acd97238870f9054a9729bc34cf2ff0d4ed8cac4ad` |

The production installer still reads the pinned manifest. These fixtures do
not add test data to the installed desktop image.
