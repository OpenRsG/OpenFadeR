#!/usr/bin/env python3
"""Rebuilds the NEURAL key detector's ONNX file (src/analysis/key/neural.rs) from its published weights.

Weights: `checkpoints/keynet.pt` of github.com/a1ex90/MusicalKeyCNN (MIT licence, commit 61d8fc1), a PyTorch state dict of
the key CNN of Korzeniowski & Widmer, "Genre-Agnostic Key Classification With Convolutional Neural Networks" (ISMIR 2018).
The network definition is that repository's `model.py` (MIT), put next to this script.

    pip install torch onnx onnxscript
    python3 keynet_export.py keynet.pt keynet.onnx

Input `cqt` [1, 1, 105, T] f32 (log-magnitude CQT, T frames of 0.2 s), output `logits` [1, 24] (global average over the
track). T is dynamic. The sha256 of the result is `neural::SHA256`.
"""
import sys

import torch
from model import KeyNet

src, dst = sys.argv[1:3]
net = KeyNet(num_classes=24, in_channels=1, Nf=20)
net.load_state_dict(torch.load(src, map_location="cpu"))
net.eval()
torch.onnx.export(
    net,
    torch.zeros(1, 1, 105, 100),
    dst,
    input_names=["cqt"],
    output_names=["logits"],
    dynamic_axes={"cqt": {3: "frames"}},
    opset_version=17,
    dynamo=False,
)
print("wrote", dst)
