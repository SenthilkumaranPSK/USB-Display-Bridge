plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.usbdisplaybridge.extend"
    // 34 (not the newer 36.1 SDK extension platform also installed on this
    // machine) -- a plain, well-supported integer API level is one less
    // thing to debug in a first build. The real test device is API 31.
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.usbdisplaybridge.extend"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }
}
