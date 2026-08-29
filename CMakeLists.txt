# IDE organization only -- the actual build is handled by Corrosion in the parent project,
# matching how patternsleuth_bind is wired.
#
# organize_targets/make_rust_visible are UE4SS helpers, so guard on them: this file must not
# break when the repo is used outside a UE4SS tree.

if(COMMAND organize_targets)
    organize_targets("^_?cargo-.*_mcp_bind" "rust/mcp_bind")
endif()

if(COMMAND make_rust_visible AND ENABLE_IDE_SOURCE_VISIBILITY)
    make_rust_visible(
        rust_ide_mcp_bind
        "${CMAKE_CURRENT_SOURCE_DIR}/src"
        "deps/first/rust/mcp_bind"
    )
endif()
