// Renders Apple's macOS app-icon shape (continuous-corner rounded square, 824pt body
// on a 1024pt canvas) as a white-on-transparent PNG mask.
// Usage: swift scripts/squircle-mask.swift out.png
import AppKit
import SwiftUI

@MainActor func render() {
    let view = ZStack {
        RoundedRectangle(cornerRadius: 185.4, style: .continuous)
            .fill(Color.white)
            .frame(width: 824, height: 824)
    }
    .frame(width: 1024, height: 1024)
    let r = ImageRenderer(content: view)
    r.scale = 1
    guard let cg = r.cgImage else { fatalError("render failed") }
    let rep = NSBitmapImageRep(cgImage: cg)
    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
}
MainActor.assumeIsolated { render() }
