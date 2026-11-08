/*
    SPDX-FileCopyrightText: 2026 the makshell contributors

    SPDX-License-Identifier: GPL-3.0-or-later
*/

// makshell's lock screen, like macOS's: the wallpaper, the date and a big clock at the top, and at the
// bottom who's logged in, with a capsule for their password. KDE's screen locker shows it, and does the
// unlocking: this only passes it what's typed, by way of `authenticator`, as Plasma's own does.

import QtCore
import QtQuick
import QtQuick.Effects

Item {
    id: root

    // What the screen locker looks for.
    property bool debug: false
    property string notification
    signal clearPassword()
    signal notificationRepeated()
    property bool viewVisible: false

    implicitWidth: 800
    implicitHeight: 600

    // Points of a 1080-point-tall screen, which is what the sizes below are measured on.
    readonly property real u: Math.max(0.6, height / 1080)
    readonly property string family: interRegular.status === FontLoader.Ready ? interRegular.font.family : "sans-serif"
    // After a wrong password, nothing's taken for a moment. Plasma 6.7 says so as graceLocked, 6.8
    // as inPasswordDelay; on either, the other is undefined, which counts as false.
    readonly property bool waiting: authenticator.graceLocked || authenticator.inPasswordDelay || graceTimer.running
    // What was typed and sent before PAM asked for it, to give it when it does.
    property string pending: ""
    property date now: new Date()
    // There was nothing to type, like with a fingerprint: it's unlocked, once that's pressed.
    property bool unlockedWithoutPassword: false

    FontLoader { id: interRegular; source: "../fonts/Inter-Regular.ttf" }
    FontLoader { source: "../fonts/Inter-SemiBold.ttf" }
    FontLoader { source: "../fonts/Inter-Bold.ttf" }

    Timer {
        interval: 1000
        running: true
        repeat: true
        onTriggered: root.now = new Date()
    }

    Connections {
        target: authenticator
        function onFailed(kind) {
            // Fingerprints and smart cards fail by themselves, and keep trying.
            if (kind !== 0) {
                return;
            }
            root.pending = "";
            password.text = "";
            shake.restart();
            graceTimer.restart();
        }
        function onSucceeded() {
            if (authenticator.hadPrompt) {
                Qt.quit();
            } else {
                root.unlockedWithoutPassword = true;
            }
        }
        function onInfoMessageChanged() { root.notification = authenticator.infoMessage; }
        function onErrorMessageChanged() { root.notification = authenticator.errorMessage; }
        function onPromptForSecretChanged() {
            password.forceActiveFocus();
            if (authenticator.promptForSecret && root.pending.length > 0) {
                authenticator.respond(root.pending);
                root.pending = "";
            }
        }
    }

    // Sends what's typed: right away if PAM is asking for it, or once it does. Since Plasma 6.8, PAM
    // runs in a worker that takes a moment to start, and asking before then does nothing, so this asks
    // again.
    function submit() {
        if (password.text.length === 0 || root.waiting) {
            return;
        }
        root.notification = "";
        if (authenticator.promptForSecret) {
            authenticator.respond(password.text);
        } else {
            root.pending = password.text;
            authenticator.startAuthenticating();
        }
    }

    // Asks until PAM is there to ask for the password, so that it's ready before anything's typed.
    Timer {
        interval: 300
        repeat: true
        running: !authenticator.promptForSecret && !root.waiting && !root.unlockedWithoutPassword
        onTriggered: authenticator.startAuthenticating()
    }
    onClearPassword: password.text = ""

    // Asking again, a moment after a wrong password, the way Plasma's does.
    Timer {
        id: graceTimer
        interval: 3000
        onTriggered: {
            root.notification = "";
            authenticator.startAuthenticating();
        }
    }
    Component.onCompleted: {
        authenticator.startAuthenticating();
        password.forceActiveFocus();
    }

    // ── The wallpaper: the shell keeps a link to it, and without one Plasma's shows from under this.
    Image {
        id: wallpaper
        anchors.fill: parent
        anchors.margins: -64
        source: StandardPaths.writableLocation(StandardPaths.GenericStateLocation) + "/makshell/lock-wallpaper"
        fillMode: Image.PreserveAspectCrop
        asynchronous: true
        cache: false
        visible: false
        sourceSize.width: root.width + 128
        sourceSize.height: root.height + 128
    }
    MultiEffect {
        // Past the edges, which blurring would otherwise pull in from nothing.
        anchors.fill: parent
        anchors.margins: -64
        source: wallpaper
        visible: wallpaper.status === Image.Ready
        // It goes out of focus while a password's being typed, as macOS's login window does.
        blurEnabled: true
        blurMax: 64
        blur: password.text.length > 0 ? 1.0 : 0.0
        brightness: password.text.length > 0 ? -0.08 : 0.0
        Behavior on blur { NumberAnimation { duration: 350; easing.type: Easing.OutCubic } }
        Behavior on brightness { NumberAnimation { duration: 350; easing.type: Easing.OutCubic } }
    }

    // ── The date and the time.
    Column {
        id: clock
        anchors.horizontalCenter: parent.horizontalCenter
        y: 70 * root.u
        spacing: -10 * root.u

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: Qt.formatDate(root.now, "dddd d MMMM")
            color: Qt.rgba(1, 1, 1, 0.92)
            font.family: root.family
            font.weight: Font.DemiBold
            font.pixelSize: 26 * root.u
        }
        Text {
            id: time
            anchors.horizontalCenter: parent.horizontalCenter
            // As the menu bar has it.
            text: Qt.formatTime(root.now, "H:mm")
            color: Qt.rgba(1, 1, 1, 0.86)
            font.family: root.family
            font.weight: Font.Bold
            font.pixelSize: 172 * root.u
            font.letterSpacing: -4 * root.u
            layer.enabled: true
            layer.effect: MultiEffect {
                shadowEnabled: true
                shadowColor: Qt.rgba(0, 0, 0, 0.35)
                shadowBlur: 0.8
                shadowVerticalOffset: 2
            }
        }
    }

    // ── Who's logged in, and their password.
    Column {
        id: user
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom
        anchors.bottomMargin: 96 * root.u
        spacing: 12 * root.u

        Item {
            id: avatar
            anchors.horizontalCenter: parent.horizontalCenter
            width: 72 * root.u
            height: width

            Image {
                id: face
                anchors.fill: parent
                source: kscreenlocker_userImage !== "" ? "file://" + kscreenlocker_userImage : ""
                fillMode: Image.PreserveAspectCrop
                sourceSize.width: width * 2
                sourceSize.height: height * 2
                visible: false
            }
            Rectangle {
                id: round
                anchors.fill: parent
                radius: width / 2
                visible: false
                layer.enabled: true
            }
            MultiEffect {
                anchors.fill: parent
                source: face
                maskEnabled: true
                maskSource: round
                visible: face.status === Image.Ready
            }
            // Without a picture, the first letter of their name, as macOS does.
            Rectangle {
                anchors.fill: parent
                radius: width / 2
                visible: face.status !== Image.Ready
                gradient: Gradient {
                    GradientStop { position: 0; color: "#a5a5ad" }
                    GradientStop { position: 1; color: "#7c7c84" }
                }
                Text {
                    anchors.centerIn: parent
                    text: kscreenlocker_userName.length > 0 ? kscreenlocker_userName[0].toUpperCase() : ""
                    color: "white"
                    font.family: root.family
                    font.weight: Font.DemiBold
                    font.pixelSize: parent.height * 0.45
                }
            }
        }

        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: kscreenlocker_userName
            color: "white"
            font.family: root.family
            font.weight: Font.DemiBold
            font.pixelSize: 16 * root.u
            layer.enabled: true
            layer.effect: MultiEffect {
                shadowEnabled: true
                shadowColor: Qt.rgba(0, 0, 0, 0.4)
                shadowBlur: 0.5
            }
        }

        // The password, in a glass capsule, which shakes when it's wrong.
        Rectangle {
            id: capsule
            anchors.horizontalCenter: parent.horizontalCenter
            width: 210 * root.u
            height: 34 * root.u
            radius: height / 2
            color: Qt.rgba(1, 1, 1, password.activeFocus ? 0.22 : 0.16)
            border.color: Qt.rgba(1, 1, 1, 0.28)
            border.width: 1
            visible: !root.unlockedWithoutPassword
            opacity: root.waiting ? 0.5 : 1.0
            Behavior on opacity { NumberAnimation { duration: 200 } }

            transform: Translate { id: shakeOffset }
            SequentialAnimation {
                id: shake
                NumberAnimation { target: shakeOffset; property: "x"; to: -14 * root.u; duration: 50 }
                NumberAnimation { target: shakeOffset; property: "x"; to: 12 * root.u; duration: 70 }
                NumberAnimation { target: shakeOffset; property: "x"; to: -9 * root.u; duration: 70 }
                NumberAnimation { target: shakeOffset; property: "x"; to: 6 * root.u; duration: 70 }
                NumberAnimation { target: shakeOffset; property: "x"; to: 0; duration: 60 }
            }

            TextInput {
                id: password
                anchors.left: parent.left
                anchors.right: go.left
                anchors.leftMargin: 14 * root.u
                anchors.rightMargin: 6 * root.u
                anchors.verticalCenter: parent.verticalCenter
                echoMode: TextInput.Password
                passwordCharacter: "●"
                passwordMaskDelay: 0
                color: "white"
                selectionColor: Qt.rgba(1, 1, 1, 0.3)
                font.family: root.family
                font.pixelSize: 14 * root.u
                font.letterSpacing: 2 * root.u
                clip: true
                enabled: !root.waiting
                focus: true
                // Nothing else here takes typing, so wherever it's typed it comes here.
                onActiveFocusChanged: if (!activeFocus && !root.unlockedWithoutPassword) forceActiveFocus()
                Keys.onEscapePressed: text = ""
                onAccepted: root.submit()

                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    visible: password.text.length === 0
                    text: "Enter Password"
                    color: Qt.rgba(1, 1, 1, 0.6)
                    font.family: root.family
                    font.pixelSize: 14 * root.u
                }
            }

            // A way to send it with the pointer, once there's something to send.
            Rectangle {
                id: go
                anchors.right: parent.right
                anchors.rightMargin: 4 * root.u
                anchors.verticalCenter: parent.verticalCenter
                width: parent.height - 8 * root.u
                height: width
                radius: width / 2
                color: Qt.rgba(1, 1, 1, goArea.containsMouse ? 0.35 : 0.24)
                opacity: password.text.length > 0 ? 1 : 0
                Behavior on opacity { NumberAnimation { duration: 150 } }
                Text {
                    anchors.centerIn: parent
                    text: "→"
                    color: "white"
                    font.family: root.family
                    font.weight: Font.DemiBold
                    font.pixelSize: parent.height * 0.62
                }
                MouseArea {
                    id: goArea
                    anchors.fill: parent
                    hoverEnabled: true
                    enabled: password.text.length > 0
                    onClicked: password.accepted()
                }
            }
        }

        // With nothing to type, a button to go in by.
        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            visible: root.unlockedWithoutPassword
            width: 120 * root.u
            height: 34 * root.u
            radius: height / 2
            color: Qt.rgba(1, 1, 1, unlockArea.containsMouse ? 0.3 : 0.2)
            border.color: Qt.rgba(1, 1, 1, 0.28)
            Text {
                anchors.centerIn: parent
                text: "Unlock"
                color: "white"
                font.family: root.family
                font.weight: Font.DemiBold
                font.pixelSize: 14 * root.u
            }
            MouseArea {
                id: unlockArea
                anchors.fill: parent
                hoverEnabled: true
                onClicked: Qt.quit()
            }
        }

        // What PAM has to say, or Caps Lock.
        Text {
            anchors.horizontalCenter: parent.horizontalCenter
            width: 360 * root.u
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            text: root.notification
            visible: text.length > 0
            color: Qt.rgba(1, 1, 1, 0.8)
            font.family: root.family
            font.pixelSize: 12 * root.u
        }
    }
}
