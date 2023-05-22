# Changelog

All notable changes to this project will be documented in this file. See [standard-version](https://github.com/conventional-changelog/standard-version) for commit guidelines.

## 0.1.0-rc.1 (2023-05-22)


### Features

* added autonomous wire support and redesigned examples ([c802a5c](https://github.com/framesurge/ipfi/commit/c802a5ca3b4702b347e9c1e53a97c49a6cf4c807))
* added project infrastructure ([14f5b50](https://github.com/framesurge/ipfi/commit/14f5b50bb4146bfad5bccabce59a93277ce4dc34))
* added support for module-style wires ([bb3a58b](https://github.com/framesurge/ipfi/commit/bb3a58bbfab61baf3ec682e88d02e57bca0bbdfe))
* added tcp server example ([d89e363](https://github.com/framesurge/ipfi/commit/d89e36371ee904246ff9f009cde1c091c2868034))
* created full interface and buffer wire systems ([2ef61d6](https://github.com/framesurge/ipfi/commit/2ef61d6ad5fe505cd66f54b5a9955c449646a207))
* made `serde` and `wire` features that can be disabled ([9803324](https://github.com/framesurge/ipfi/commit/98033245bdb655f7caabce33c826cffed3063a38))
* made interface dynamically allocate new messages ([d8d04a5](https://github.com/framesurge/ipfi/commit/d8d04a5175fb20f7344d7f2d32e25f07933be78a))
* made ipfibuf use explicit terminator signal ([bc226d4](https://github.com/framesurge/ipfi/commit/bc226d49baffb0af1afdb205e5242b428c6125f9))
* made response messages use procedure/call indices instead of direct response indices ([34bfb06](https://github.com/framesurge/ipfi/commit/34bfb06fe00ae15fc0baebb616d39532a77bc445))
* made some interface maps auto-clear ([4f34171](https://github.com/framesurge/ipfi/commit/4f34171fb29016d91a270fb649869d7fe288d9cf))
* removed wasm spinlock ([2f54bed](https://github.com/framesurge/ipfi/commit/2f54bed7114eabe77f47c43bdfa56d2924877feb))
* simplified wire system and added procedure calling methods ([365c49b](https://github.com/framesurge/ipfi/commit/365c49b5ab002ea730563af60d43ea2e3c069629))
* wrote interface procedure methods ([27c0459](https://github.com/framesurge/ipfi/commit/27c0459b97db8e919d0d943b3c8ed045b112fdd2))


### Bug Fixes

* fixed bug in rpc system ([28bf977](https://github.com/framesurge/ipfi/commit/28bf9770d1f5443ec2871e7e912e9b8c7d706c6d))
* fixed no-argument procedures ([9b14865](https://github.com/framesurge/ipfi/commit/9b1486578086e673f4c1ab2898b870a4ec4dec00))
* made `.fill()` respect termination as it does end-of-input ([a62e44e](https://github.com/framesurge/ipfi/commit/a62e44e2e4f865d0d403226f2f7297ba2c0be844))
