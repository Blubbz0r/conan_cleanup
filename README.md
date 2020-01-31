# conan_cleanup

[![Build Status](https://travis-ci.com/Blubbz0r/conan_cleanup.svg?branch=master)](https://travis-ci.com/Blubbz0r/conan_cleanup)

Command-line tool helping to cleanup your local [conan](https://conan.io/) cache.

Given a path to the root directory to all projects using conan, the tool parses all conaninfo.txt files for the used packages and compares them to all packages in your local cache (using `conan search`). All packages not used by any project can then be removed either with manual confirmation (default) or fully automatically.