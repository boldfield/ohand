//! Parsing of headers and `nm` output, and the header-versus-library comparison.

use ohand_bindgen::{compare_header_with_library, declared_functions, exported_functions};

const HEADER: &str = r#"
/* ohand_in_comment(void); */
#define OHAND_FLAG 1
typedef struct OhandResult {
  uint32_t status;
} OhandResult;
typedef void (*ohand_callback)(int value);
uint32_t ohand_version(void);
struct OhandResult ohand_save(const uint8_t *request,
                              size_t request_len);
"#;

#[test]
fn declared_functions_ignore_comments_typedefs_and_defines() {
    let names: Vec<String> = declared_functions(HEADER).into_iter().collect();
    assert_eq!(names, ["ohand_save", "ohand_version"]);
}

#[test]
fn exported_functions_read_elf_and_mach_o_output() {
    let elf = "0000000000000000 T ohand_save\n0000000000000010 t ohand_local\n                 U ohand_other\n0000000000000020 T _ZN4rust\n";
    let mach_o = "0000000000003f00 T _ohand_save\n0000000000003f10 D _ohand_data\nlibohand_bindings.a(x.o):\n";
    assert_eq!(
        exported_functions(elf).into_iter().collect::<Vec<_>>(),
        ["ohand_save"]
    );
    assert_eq!(
        exported_functions(mach_o).into_iter().collect::<Vec<_>>(),
        ["ohand_save"]
    );
}

#[test]
fn comparison_reports_both_directions_of_drift() {
    let declared = declared_functions(HEADER);
    assert!(compare_header_with_library(&declared, &declared).is_ok());

    let mut library_missing_one = declared.clone();
    library_missing_one.remove("ohand_save");
    let error = compare_header_with_library(&declared, &library_missing_one).unwrap_err();
    assert!(
        error.contains("declared but not exported: [\"ohand_save\"]"),
        "{error}"
    );

    let mut library_has_extra = declared.clone();
    library_has_extra.insert("ohand_undeclared".to_string());
    let error = compare_header_with_library(&declared, &library_has_extra).unwrap_err();
    assert!(
        error.contains("exported but not declared: [\"ohand_undeclared\"]"),
        "{error}"
    );

    assert!(compare_header_with_library(&Default::default(), &Default::default()).is_err());
}

#[test]
fn comparison_rejects_exports_outside_the_ohand_namespace() {
    let declared = declared_functions("int unprefixed_export(void);\nint ohand_ok(void);");
    let error = compare_header_with_library(&declared, &declared).unwrap_err();
    assert!(error.contains("unprefixed_export"), "{error}");
    assert!(!error.contains("ohand_ok"), "{error}");
}
