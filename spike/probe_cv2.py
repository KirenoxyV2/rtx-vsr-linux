import cv2, numpy as np, sys
print("cv2", cv2.__version__)
# Can we decode + encode video via OpenCV's bundled FFmpeg?
cap = cv2.VideoCapture(sys.argv[1] if len(sys.argv) > 1 else "")
if cap.isOpened():
    n = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))
    w, h = int(cap.get(3)), int(cap.get(4))
    print(f"decode OK: {w}x{h}, {n} frames")
    ok, frame = cap.read()
    print("read frame:", ok, None if not ok else frame.shape)
else:
    # no file given — just confirm a writer can be constructed (codec present)
    vw = cv2.VideoWriter("/tmp/_vsr_probe.mp4", cv2.VideoWriter_fourcc(*"mp4v"), 30, (640, 360))
    print("VideoWriter(mp4v) constructible:", vw.isOpened())
    if vw.isOpened():
        for _ in range(5):
            vw.write(np.zeros((360, 640, 3), np.uint8))
        vw.release()
        import os
        print("wrote", os.path.getsize("/tmp/_vsr_probe.mp4"), "bytes")
