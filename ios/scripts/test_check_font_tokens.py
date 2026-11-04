import unittest

import check_font_tokens as checker


def source(*tokens):
    return "enum DSFont {\n" + "\n".join("    " + token for token in tokens) + "\n}\n"


class CheckFontTokensTests(unittest.TestCase):
    def test_repository_tokens_pass(self):
        self.assertEqual(checker.problems(checker.DEFAULT_SOURCE.read_text()), [])

    def test_accepts_text_style_forms(self):
        text = source(
            'static let serifName = "Serif"',
            "static var a: Font { .custom(serifName, size: 28, relativeTo: .title) }",
            "static var b: Font { Font.custom(serifName, size: 76, relativeTo: .largeTitle).monospacedDigit() }",
            "static func c(size: CGFloat) -> Font { .custom(serifName, size: size, relativeTo: .largeTitle) }",
            "static let d: Font = .body",
            "static let e: Font = .system(.footnote, design: .default).weight(.semibold)",
            "static let f: Font = e.italic()",
        )
        self.assertEqual(checker.problems(text), [])

    def test_rejects_fixed_system_size(self):
        errors = checker.problems(source("static let fixed: Font = .system(size: 17)"))
        self.assertEqual(len(errors), 1)
        self.assertIn("DSFont.fixed", errors[0])

    def test_rejects_custom_without_relative_to(self):
        errors = checker.problems(source('static var fixed: Font { .custom("Serif", size: 28) }'))
        self.assertEqual(len(errors), 1)

    def test_rejects_custom_name_constant_without_relative_to(self):
        errors = checker.problems(source(
            'static let serifName = "Serif"',
            "static var fixed: Font { .custom(serifName, size: 22) }",
        ))
        self.assertEqual(len(errors), 1)
        self.assertIn("DSFont.fixed", errors[0])

    def test_missing_enum_is_an_error(self):
        with self.assertRaises(ValueError):
            checker.problems("enum Other {}")


if __name__ == "__main__":
    unittest.main()
