// VRAM guard: DXGI queries the default adapter's dedicated budget.
// Pipeline checks before each chunk; over budget -> shrink batch / wait.
#include "internal.h"
// NOTE: WIN32_LEAN_AND_MEAN comes from CMake compile definitions (see above).
#include <windows.h>
#include <dxgi1_4.h>
#include <wrl/client.h>

namespace pv {

float vram_usage_fraction() {
    using Microsoft::WRL::ComPtr;
    ComPtr<IDXGIFactory4> f;
    if (FAILED(CreateDXGIFactory1(IID_PPV_ARGS(&f)))) return -1;
    // Iterate adapters and guard the one with the largest local budget
    // (discrete NVIDIA/AMD GPU, not the iGPU) — adapter 0 is often wrong
    // on dual-GPU laptops.
    float worst = -1;
    for (UINT i = 0;; ++i) {
        ComPtr<IDXGIAdapter3> a;
        if (FAILED(f->EnumAdapters(i, reinterpret_cast<IDXGIAdapter**>(a.GetAddressOf()))))
            break;
        // NOTE: GetDesc1 (not GetDesc): only DXGI_ADAPTER_DESC1 carries
        // Flags; the v1 struct has VendorId but no Flags member.
        DXGI_ADAPTER_DESC1 desc{};
        if (FAILED(a->GetDesc1(&desc))) continue;
        // Skip the Microsoft Basic Render Driver (VendorId 0x1414) and other
        // software adapters: their counters report ~0 usage against huge or
        // zero budgets, which pinned readings at 0.00-0.01 live and blinded
        // the guard on machines that do have a real GPU.
        if (desc.VendorId == 0x1414 || (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) continue;
        DXGI_QUERY_VIDEO_MEMORY_INFO info{};
        if (FAILED(a->QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &info)))
            continue;
        // Drivers sometimes report Budget == 0; fall back to the adapter's
        // dedicated memory as the denominator rather than going blind.
        unsigned long long budget = info.Budget;
        if (!budget) {
            if (!desc.DedicatedVideoMemory) continue;
            budget = desc.DedicatedVideoMemory;
        }
        float u = (float)info.CurrentUsage / (float)budget;
        if (u > worst) worst = u;
    }
    return worst;
}

bool vram_over_budget(int budget_pct) {
    float u = vram_usage_fraction();
    if (u < 0) return false;  // unknown -> proceed
    return u * 100.0f >= (float)budget_pct;
}

}  // namespace pv
