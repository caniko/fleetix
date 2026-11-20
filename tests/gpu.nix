let
  gpu = import ../lib/gpu.nix;
  node = "/dev/dri/by-path/pci-0000:03:00.0-render";
  mediaNode = "/dev/dri/by-path/pci-0000:65:00.0-render";
  actual = gpu.routes {
    igpu = "intel";
    dgpu = "amd";
    render.renderNode = node;
    media = {
      vendor = "intel";
      renderNode = mediaNode;
      libvaDriver = "iHD";
    };
    compute.backend = "rocm";
  };
  rejects = record: !(builtins.tryEval (builtins.deepSeq (gpu.routes record) true)).success;
  igpu = {
    address = "0000:7d:00.0";
    id = "1002:13c0";
    vendor = "amd";
    rom = "vfct";
  };
in
  assert (gpu.routes {passthrough.igpu = igpu;}).passthrough
  == {
    igpu = igpu // {repeatable = false;};
  };
  assert (gpu.routes {passthrough.igpu = removeAttrs igpu ["rom"];}).passthrough.igpu.rom == "device";
  assert rejects {passthrough.Bad = igpu;};
  assert rejects {passthrough.igpu = igpu // {id = "1002";};};
  assert rejects {passthrough.igpu = igpu // {rom = "flash";};};
  assert rejects {
    passthrough = {
      a = igpu;
      b = igpu;
    };
  };
  assert rejects {
    render.renderNode = "/dev/dri/by-path/pci-0000:7D:00.0-render";
    passthrough.igpu = igpu;
  };
  assert rejects {
    igpu = "amd";
    media = {
      vendor = "amd";
      renderNode = "/dev/dri/by-path/pci-0000:7d:00.0-render";
      libvaDriver = "radeonsi";
    };
    passthrough.igpu = igpu;
  };
  assert gpu.noGpu.passthrough == {};
  assert rejects {igpu = "unknown";};
  assert rejects {render.renderNode = "/dev/dri/renderD128";};
  assert rejects {
    dgpu = "intel";
    compute.backend = "rocm";
  };
  assert rejects {compute.backend = "unknown";};
  assert rejects {
    dgpu = "amd";
    media = {
      vendor = "intel";
      renderNode = mediaNode;
      libvaDriver = "iHD";
    };
  };
  assert rejects {
    dgpu = "amd";
    media = {
      vendor = "amd";
      renderNode = node;
      libvaDriver = " ";
    };
  };
  assert builtins.all (case: gpu.validRenderNode case.node == (case.selector != null)) gpu.contract.cases;
  assert actual.render
  == {
    enable = true;
    renderNode = node;
  };
  assert actual.media
  == {
    enable = true;
    vendor = "intel";
    renderNode = mediaNode;
    libvaDriver = "iHD";
  };
  assert actual.compute == {backend = "rocm";};
  assert !gpu.noGpu.routes.render.enable && !gpu.noGpu.routes.media.enable && gpu.noGpu.compute == null; true
