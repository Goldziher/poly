# Reference formatter: palantir-java-format (Palantir's opinionated fork of
# google-java-format). Its Main reads stdin and writes stdout via `-`, but needs
# guava on the classpath and jdk.compiler internals exported.
FROM eclipse-temurin:21-jdk-alpine
ADD https://repo1.maven.org/maven2/com/palantir/javaformat/palantir-java-format/2.50.0/palantir-java-format-2.50.0.jar /opt/pjf.jar
ADD https://repo1.maven.org/maven2/com/google/guava/guava/33.2.1-jre/guava-33.2.1-jre.jar /opt/guava.jar
ADD https://repo1.maven.org/maven2/com/google/guava/failureaccess/1.0.2/failureaccess-1.0.2.jar /opt/failureaccess.jar
RUN printf '#!/bin/sh\nexec java \
--add-exports jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED \
--add-exports jdk.compiler/com.sun.tools.javac.file=ALL-UNNAMED \
--add-exports jdk.compiler/com.sun.tools.javac.parser=ALL-UNNAMED \
--add-exports jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED \
--add-exports jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED \
-cp /opt/pjf.jar:/opt/guava.jar:/opt/failureaccess.jar com.palantir.javaformat.java.Main -\n' \
    > /usr/local/bin/fmt && chmod +x /usr/local/bin/fmt
ENTRYPOINT ["/usr/local/bin/fmt"]
