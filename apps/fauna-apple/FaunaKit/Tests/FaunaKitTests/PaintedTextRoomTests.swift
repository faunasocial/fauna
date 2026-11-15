import CoreGraphics
import Testing
@testable import FaunaKit

// Pins for convention 17's `painted-text-has-area` room rule on macOS + iOS
// (`AutomationPaintedTextRoom.room`): the ROOM the layout gave an element, cut
// by clipping ancestors, a scroll viewport excepted.

private func anc(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat,
                 _ kind: AutomationRoomAncestor.Kind = .plain) -> AutomationRoomAncestor {
    AutomationRoomAncestor(box: CGRect(x: x, y: y, width: w, height: h), kind: kind)
}

@Suite struct PaintedTextRoomTests {
    @Test func aTextGivenNoWidthIsStarvedInAPaneWithArea() {
        let room = AutomationPaintedTextRoom.room(
            own: CGRect(x: 180, y: 10, width: 0, height: 17),
            ancestors: [anc(180, 10, 0, 17), anc(0, 0, 185, 27)])
        #expect(room == AutomationLaidOutRoom(width: 0, height: 17, pane: CGSize(width: 185, height: 27), depth: 1))
    }

    @Test func aClippingAncestorCutsTheRoom() {
        // A text laid out wider than a clipping row that sits beside it.
        let room = AutomationPaintedTextRoom.room(
            own: CGRect(x: 200, y: 0, width: 80, height: 20),
            ancestors: [anc(0, 0, 200, 20, .clip)])
        #expect(room?.width == 0)
        #expect(room?.height == 20)
    }

    @Test func anUnclippedOverflowKeepsItsBox() {
        let room = AutomationPaintedTextRoom.room(
            own: CGRect(x: 200, y: 0, width: 80, height: 20),
            ancestors: [anc(0, 0, 200, 20)])
        #expect(room?.width == 80)
    }

    @Test func aTextScrolledOutOfItsViewportKeepsItsBox() {
        // Below the fold of a 300-pt viewport, inside a clipping window-sized view.
        let room = AutomationPaintedTextRoom.room(
            own: CGRect(x: 0, y: 5000, width: 100, height: 20),
            ancestors: [anc(0, 0, 400, 6000), anc(0, 0, 400, 300, .scroll), anc(0, 0, 400, 900, .clip)])
        #expect(room?.width == 100)
        #expect(room?.height == 20)
    }

    @Test func aViewportGivenNoHeightStarvesWhatItHolds() {
        let room = AutomationPaintedTextRoom.room(
            own: CGRect(x: 0, y: 10, width: 100, height: 20),
            ancestors: [anc(0, 0, 400, 6000), anc(0, 10, 400, 0, .scroll), anc(0, 0, 400, 900)])
        #expect(room?.height == 0)
        #expect(room?.width == 100)
    }

    @Test func aZeroSizeCarrierIsNotPublished() {
        // macOS's hidden `page-heading` carrier: `.frame(width: 0, height: 0)`.
        #expect(AutomationPaintedTextRoom.room(
            own: CGRect(x: 10, y: 0, width: 0, height: 0),
            ancestors: [anc(0, 0, 800, 600)]) == nil)
    }

    @Test func noAncestorWithAreaLeavesNoPane() {
        let room = AutomationPaintedTextRoom.room(
            own: CGRect(x: 0, y: 0, width: 0, height: 17), ancestors: [anc(0, 0, 0, 17)])
        #expect(room?.pane == nil)
        #expect(room?.depth == 0)
    }
}
