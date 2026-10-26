#!/usr/bin/env python3
"""Exports the Basic Pitch weights (Spotify, Apache-2.0) from its ONNX model into the flat
little-endian f32 file `crates/notes/models/basic_pitch.bin` read by `cadenza_notes::poly`.

Usage: python3 export.py nmp.onnx crates/notes/models/basic_pitch.bin
(nmp.onnx: basic_pitch/saved_models/icassp_2022/nmp.onnx of github.com/spotify/basic-pitch;
needs the `onnx` and `numpy` packages.) With --reference out.bin [--onnxruntime] it also
writes the model's outputs for a fixed test signal, for the parity test.

File layout: b"CDZBP1", u32 tensor count, then per tensor: u32 element count followed by
that many f32. Tensors in this order (shapes as in the ONNX, OIHW for convolutions):
cqt_a [36,256], cqt_b [36,256], lowpass [256], bin_norm [309], bn_scale [1], bn_shift [1],
conv_harm w [32,8,5,5] b [32], conv_contour1 w [8,8,3,39] b [8], conv_contour2 w [1,8,5,5]
b [1], conv_note1 w [32,1,7,7] b [32], conv_note2 w [1,32,7,3] b [1], conv_onset w [1,33,3,3]
b [1].
"""
import struct
import sys

import numpy as np
import onnx
from onnx import numpy_helper


def tensors(model):
    inits = {t.name: numpy_helper.to_array(t) for t in model.graph.initializer}

    def find(suffix):
        hits = [v for k, v in inits.items() if k.endswith(suffix)]
        assert len(hits) == 1, suffix
        return hits[0]

    convs = {}
    for n in model.graph.node:
        if n.op_type == "Conv" and n.input[1] not in convs:
            convs[n.input[1]] = (inits[n.input[1]], inits[n.input[2]])
    by_shape = {w.shape: (w, b) for w, b in convs.values() if w.shape[2] != 1}
    out = [
        find("const_fold_opt__655").reshape(36, 256),
        find("const_fold_opt__664").reshape(36, 256),
        find("const_fold_opt__734").reshape(256),
        find("cq_t2010v2_1/Sqrt").reshape(309),
        find("FusedBatchNormV3").reshape(1),
        find("FusedBatchNormV31").reshape(1),
    ]
    for shape in [(32, 8, 5, 5), (8, 8, 3, 39), (1, 8, 5, 5), (32, 1, 7, 7), (1, 32, 7, 3), (1, 33, 3, 3)]:
        w, b = by_shape[shape]
        out += [w, b]
    return out


def write(path, arrays):
    with open(path, "wb") as f:
        f.write(b"CDZBP1")
        f.write(struct.pack("<I", len(arrays)))
        for a in arrays:
            a = np.ascontiguousarray(a, dtype="<f4").ravel()
            f.write(struct.pack("<I", a.size))
            f.write(a.tobytes())


def test_signal():
    """2 s at 22.05 kHz: a C major triad (C4 E4 G4), harmonics, then A2 alone."""
    sr, n = 22050, 43844
    t = np.arange(n) / sr
    x = np.zeros(n)
    for midi, start, end in [(60, 0.1, 1.0), (64, 0.1, 1.0), (67, 0.1, 1.0), (45, 1.1, 1.9)]:
        f = 440 * 2 ** ((midi - 69) / 12)
        env = ((t >= start) & (t < end)) * np.exp(-(t - start).clip(0) * 1.5)
        for h in range(1, 6):
            x += env * np.sin(2 * np.pi * f * h * t) * 0.25 / h
    return x.astype(np.float32)


def main():
    model = onnx.load(sys.argv[1])
    arrays = tensors(model)
    write(sys.argv[2], arrays)
    print(f"{sys.argv[2]}: {sum(a.size for a in arrays)} weights")
    if "--reference" in sys.argv:
        import onnxruntime as ort

        path = sys.argv[sys.argv.index("--reference") + 1]
        x = test_signal()
        sess = ort.InferenceSession(sys.argv[1])
        outs = sess.run(None, {sess.get_inputs()[0].name: x.reshape(1, -1, 1)})
        names = [o.name for o in sess.get_outputs()]
        by = dict(zip(names, outs))
        # contour [172,264], note [172,88], onset [172,88]
        write(path, [x, by["StatefulPartitionedCall:0"][0], by["StatefulPartitionedCall:1"][0], by["StatefulPartitionedCall:2"][0]])
        print(f"{path}: reference outputs")


main()
