# ClawChat — React Native Mobile App

Real-time chat client for SquirrelChat (ClawChat), built with React Native 0.74.

## Prerequisites

- Node.js 18+
- Watchman (macOS: `brew install watchman`)
- **iOS**: Xcode 15+, CocoaPods (`gem install cocoapods`)
- **Android**: Android Studio, JDK 17, Android SDK (API 34)

## Setup

```bash
# Install JS dependencies
npm install

# iOS — install native pods
cd ios && pod install && cd ..
```

## Running

### iOS Simulator

```bash
npm run ios
# Or target a specific simulator:
npx react-native run-ios --simulator "iPhone 15 Pro"
```

### Android Emulator

```bash
# Start an AVD from Android Studio first, then:
npm run android
```

### Metro bundler (manual)

```bash
npm start
```

## Project Structure

```
ClawChat/
├── App.tsx                  # Root navigator + auth bootstrap
├── index.js                 # RN entry point
├── src/
│   ├── api/
│   │   └── client.js        # Axios client (JWT auto-attach)
│   ├── ws/
│   │   └── client.js        # WebSocket client w/ reconnect
│   ├── store/
│   │   └── auth.ts          # AsyncStorage JWT helpers
│   ├── screens/
│   │   ├── AuthScreen.tsx       # Login
│   │   ├── ChannelListScreen.tsx # Channels + DM tabs
│   │   └── MessagesScreen.tsx   # Message view + composer
│   └── components/
│       ├── MessageBubble.tsx    # Chat bubble
│       └── PresenceDot.tsx      # Online/offline indicator
```

## Navigation Flow

```
Auth → ChannelList (tabs: Channels | DMs) → Messages
```

Pressing **Sign Out** returns to the Auth screen and clears the stored JWT.

## API & WebSocket

- **REST**: `http://146.190.134.110:8793/api`
- **WebSocket**: `ws://146.190.134.110:8793/ws?token=<JWT>`

The WebSocket client reconnects automatically (up to 10 attempts, 3 s delay).

## Troubleshooting

| Issue | Fix |
|-------|-----|
| Metro not finding modules | `npm start -- --reset-cache` |
| iOS build fails (pods) | `cd ios && pod install --repo-update` |
| Android build fails | Check `ANDROID_HOME` env var and SDK path |
| Network error on device | Ensure device and API server are on the same network; update `BASE_URL` / `WS_BASE` in `src/api/client.js` and `src/ws/client.js` |
