# Loski's graphics-card voice (talking back, beta): Chatterbox, by Resemble
# AI (MIT license), on an NVIDIA card. Built on your computer by `loski build`
# when COMPOSE_PROFILES=gpu-voice is in .env; it's about 13 GB.
FROM pytorch/pytorch:2.8.0-cuda12.8-cudnn9-runtime
# Chatterbox pins torch 2.6, which doesn't know the RTX 50 series: it goes on
# this image's torch 2.8 instead, with its other packages as it lists them.
RUN pip install --no-cache-dir --no-deps chatterbox-tts==0.1.7 \
    && pip install --no-cache-dir "numpy<2.0.0,>=1.24.0" librosa==0.11.0 s3tokenizer \
        transformers==5.2.0 diffusers==0.29.0 "resemble-perth>=1.0.0" conformer==0.3.2 \
        safetensors==0.5.3 spacy-pkuseg pykakasi==2.3.0 pyloudnorm omegaconf
COPY server.py /server.py
# The model (about 3 GB) is downloaded on the first start and kept here.
ENV HF_HOME=/models
CMD ["python", "-I", "/server.py"]
